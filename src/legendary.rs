//! Thin wrapper around the legendary CLI. Everything Divine knows about Epic
//! comes from here; we never talk to Epic's API ourselves.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use anyhow::{anyhow, bail, Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::config::{self, Config};
use crate::log::warn;
use crate::models::{InstalledEntry, ListEntry};

/// How many trailing log lines we keep around to show in an error toast.
const LOG_TAIL: usize = 30;

#[derive(Debug, Clone)]
pub struct Legendary {
    bin: String,
    config_dir: PathBuf,
    /// Bytes per second downloads may use, zero for no ceiling.
    download_limit: u64,
}

/// Download progress, measured in bytes actually transferred.
///
/// legendary's own "Progress: N%" counts chunk *tasks*, and on an update most
/// chunks are copied from the existing install rather than downloaded, so that
/// number jumps to nearly 100% immediately. Bytes downloaded against the
/// reported download size is the figure that means something.
#[derive(Debug, Clone, Default)]
pub struct Progress {
    pub fraction: f64,
    pub done_mib: f64,
    pub total_mib: f64,
    pub speed_mib: f64,
    pub eta: String,
}

/// Manifest sizes, in bytes: compressed for the download, uncompressed on
/// disk. Per tag they add up to slightly more than the whole, since a file can
/// carry several tags.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ManifestInfo {
    pub download_size: u64,
    pub disk_size: u64,
    pub tag_download: HashMap<String, u64>,
    pub tag_disk: HashMap<String, u64>,
}

/// What `install` is being asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstallMode {
    /// A first install, optionally pulling in the game's add-ons with it.
    Fresh {
        with_dlcs: bool,
    },
    Update,
    Repair,
    /// Adding or dropping components on a game already on disk.
    Components,
}

impl InstallMode {
    /// Whether stopping a job of this kind can be picked up later.
    ///
    /// Adding components to a game already on disk cannot. legendary decides
    /// what to fetch by comparing manifests, and the build has not changed, so
    /// nothing looks new; the one thing that saves it is a scan for files
    /// missing on disk — and that scan is skipped whenever a resume file
    /// exists (`downloader/mp/manager.py`, the `elif resume:` branch). A
    /// stopped component download therefore leaves a resume file that makes
    /// every later run report "nothing to download". Verify and repair does
    /// finish the job, since it works from the files themselves.
    ///
    /// Worth fixing upstream: the scan should run alongside the resume data
    /// rather than instead of it. If legendary will not take the change, this
    /// is a candidate for a fork — it is the only place where Divine has to
    /// talk the user out of something rather than just do it.
    pub fn resumable(self) -> bool {
        !matches!(self, InstallMode::Components)
    }

    /// Present continuous, because it always ends up in "<verb> <game>".
    pub fn verb(self) -> &'static str {
        match self {
            InstallMode::Fresh { .. } => "Installing",
            InstallMode::Update => "Updating",
            InstallMode::Repair => "Repairing",
            InstallMode::Components => "Updating",
        }
    }
}

/// What a file check found. legendary exits 0 either way, so the numbers come
/// from its log.
#[derive(Debug, Clone, Copy, Default)]
pub struct VerifyReport {
    pub corrupted: usize,
    pub missing: usize,
}

impl VerifyReport {
    pub fn is_ok(&self) -> bool {
        self.corrupted == 0 && self.missing == 0
    }
}

/// Which way saves are allowed to move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveSync {
    Both,
    Upload,
    Download,
}

/// How a download ended. One stopped on purpose is not a failure: legendary
/// appends to a resume file as it goes, so the next run picks up where this one
/// left off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallEnd {
    Done,
    Interrupted,
}

/// Handle on a running download, so the UI can pause or cancel it.
///
/// The process group only exists a moment after the job starts, so a stop asked
/// for in that window is remembered and applied as soon as there is something to
/// signal.
#[derive(Debug, Default)]
pub struct Control {
    group: AtomicI32,
    stopped: AtomicBool,
}

impl Control {
    fn attach(&self, group: i32) {
        self.group.store(group, Ordering::SeqCst);
        if self.stopped.load(Ordering::SeqCst) {
            stop(group, false);
        }
    }

    /// SIGTERM rather than SIGKILL, so legendary gets to flush its resume file.
    pub fn request_stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        let group = self.group.load(Ordering::SeqCst);
        if group != 0 {
            stop(group, false);
        }
    }

    pub fn was_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }
}

/// What came of claiming the Ubisoft titles.
#[derive(Debug, PartialEq, Eq)]
pub enum Activation {
    /// No Ubisoft account is linked to the Epic one yet. legendary opens the
    /// page that fixes that before giving up.
    NotLinked,
    /// Every title the library has was claimed already.
    NothingLeft,
    /// What this run claimed, and what refused to be claimed.
    Claimed {
        titles: Vec<String>,
        failed: Vec<String>,
    },
}

#[derive(Debug)]
pub struct LaunchOutcome {
    pub ok: bool,
    pub log_tail: Vec<String>,
}

impl Legendary {
    pub fn new(cfg: &Config) -> Self {
        Self {
            bin: cfg.general.legendary.clone(),
            config_dir: config::legendary_config_dir(),
            download_limit: cfg.downloads.limit_bytes(),
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(&self.bin);
        // Point legendary at Divine's own state directory.
        cmd.env("LEGENDARY_CONFIG_PATH", &self.config_dir);
        // And run it from there too. legendary resolves a few paths with
        // os.path.realpath, which quietly turns anything it failed to expand
        // into a path relative to the working directory; whatever Divine was
        // started from should not be on the receiving end of that.
        cmd.current_dir(&self.config_dir);
        // Everything legendary runs inherits this too, umu and the game among
        // them, so a launch reaches the proxy as well.
        if let Some(relay) = crate::relay::address() {
            crate::relay::point(&mut cmd, &relay);
        }
        cmd.arg("-y");
        cmd.args(args);
        cmd
    }

    /// Runs a command and returns stdout, or an error carrying legendary's log tail.
    fn run(&self, args: &[&str]) -> Result<String> {
        let out = self
            .command(args)
            .output()
            .with_context(|| format!("running `{} {}`", self.bin, args.join(" ")))?;
        if !out.status.success() {
            bail!(
                "legendary {} failed:\n{}",
                args.first().copied().unwrap_or_default(),
                tail_of(&String::from_utf8_lossy(&out.stderr)).join("\n")
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn run_json<T: DeserializeOwned>(&self, args: &[&str]) -> Result<T> {
        let stdout = self.run(args)?;
        serde_json::from_str(&stdout).context("parsing legendary JSON output")
    }

    pub fn user_file(&self) -> PathBuf {
        self.config_dir.join("user.json")
    }

    /// Cheap, offline check: is there a usable session on disk?
    pub fn account(&self) -> Option<String> {
        let text = fs::read_to_string(self.user_file()).ok()?;
        let json: serde_json::Value = serde_json::from_str(&text).ok()?;
        json.get("displayName")?.as_str().map(str::to_string)
    }

    pub fn is_logged_in(&self) -> bool {
        self.account().is_some()
    }

    /// `auth --code` exits 0 even when the code is rejected, so verify by hand.
    pub fn auth(&self, code: &str) -> Result<String> {
        let out = self
            .command(&["auth", "--code", code])
            .output()
            .context("running legendary auth")?;
        self.account().ok_or_else(|| {
            anyhow!(
                "login failed:\n{}",
                tail_of(&String::from_utf8_lossy(&out.stderr)).join("\n")
            )
        })
    }

    pub fn logout(&self) -> Result<()> {
        self.run(&["auth", "--delete"]).map(|_| ())
    }

    /// Claims the library's Ubisoft titles on the linked Ubisoft account.
    ///
    /// Epic only sells those games; Ubisoft Connect holds the files, and until
    /// the entitlement is claimed there is nothing to download anywhere. The
    /// call covers the whole library at once, which is legendary's own shape
    /// for it, so it is not a per game action however it is presented.
    ///
    /// legendary exits 0 whatever happens here, so the outcome is read out of
    /// what it logged.
    pub fn activate_uplay(&self) -> Result<Activation> {
        let out = self
            .command(&["activate", "--uplay"])
            .output()
            .context("running legendary activate")?;
        Ok(read_activation(&String::from_utf8_lossy(&out.stderr)))
    }

    /// What a launch would run, without running it.
    ///
    /// Deliberately not `run`: legendary exits non-zero when it will not
    /// launch at all, and the reason it gives is the very thing worth reading,
    /// so the output comes back either way.
    pub fn dry_run(&self, app_name: &str, wrapper: &str, extra_args: &[String]) -> Result<String> {
        let mut cmd = self.command(&[
            "launch",
            app_name,
            "--no-wine",
            "--dry-run",
            "--wrapper",
            wrapper,
        ]);
        cmd.args(extra_args);
        let out = cmd.output().context("running legendary launch --dry-run")?;
        Ok(String::from_utf8_lossy(&out.stderr).into_owned())
    }

    /// Writes down that a game now lives somewhere else.
    ///
    /// legendary's own `move` is a rename and cannot cross a filesystem, which
    /// is every interesting case — btrfs even counts two subvolumes of one
    /// filesystem as separate. So Divine moves the folder and `--skip-move`
    /// asks legendary only to record where it went.
    pub fn relocate(&self, app_name: &str, new_base: &Path) -> Result<()> {
        self.run(&["move", app_name, &new_base.to_string_lossy(), "--skip-move"])
            .map(|_| ())
    }

    pub fn list(&self) -> Result<Vec<ListEntry>> {
        self.run_json(&["list", "--json"])
    }

    pub fn list_installed(&self) -> Result<Vec<InstalledEntry>> {
        self.run_json(&["list-installed", "--json"])
    }

    /// Offline fallback: legendary caches one JSON file per owned game.
    pub fn list_cached(&self) -> Result<Vec<ListEntry>> {
        Ok(self
            .cached_entries()?
            .into_iter()
            .filter(|entry| entry.metadata.is_game())
            .collect())
    }

    /// Owned add-ons. `list` leaves them out entirely, so the metadata cache is
    /// the only place they can be read from; legendary fills it in the same
    /// pass that fetches the library.
    pub fn cached_dlcs(&self) -> Vec<ListEntry> {
        self.cached_entries()
            .unwrap_or_default()
            .into_iter()
            .filter(|entry| entry.metadata.base_app_name().is_some())
            .collect()
    }

    /// Everything legendary has cached: one JSON file per owned app.
    fn cached_entries(&self) -> Result<Vec<ListEntry>> {
        let dir = self.config_dir.join("metadata");
        let mut entries = Vec::new();
        for file in fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
            let path = file?.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            if let Ok(entry) = serde_json::from_str::<ListEntry>(&text) {
                entries.push(entry);
            }
        }
        Ok(entries)
    }

    /// Downloads a game, updates one, or redownloads the files that no longer
    /// match the manifest. Blocking, and `on_progress` is called from this
    /// thread for every status line.
    pub fn install(
        &self,
        app_name: &str,
        place: Place,
        mode: InstallMode,
        tags: &[String],
        control: &Control,
        on_progress: &dyn Fn(Progress),
    ) -> Result<InstallEnd> {
        let args = install_args(app_name, place, mode, tags);

        let mut cmd = self.command(&args);
        // legendary's download workers are requests sessions, which read the
        // proxy variables. With no limit there is no lease, and the download
        // goes out the way everything else does. The lease is held for the
        // whole call, which is what keeps the relay alive exactly as long as it
        // is being used.
        let lease = crate::relay::lease(self.download_limit);
        if let Some(lease) = &lease {
            crate::relay::point(&mut cmd, lease.address());
        }
        cmd.stdout(Stdio::inherit()).stderr(Stdio::piped());
        // Its own group, so pausing reaches the worker processes legendary forks
        // to download chunks, not just legendary itself.
        cmd.process_group(0);
        let mut child = cmd.spawn().context("running legendary install")?;
        control.attach(child.id() as i32);

        let mut progress = Progress::default();
        let mut tail: Vec<String> = Vec::new();
        if let Some(stderr) = child.stderr.take() {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                crate::log::from_tool("legendary", &line);
                if update(&mut progress, &line) {
                    on_progress(progress.clone());
                }
                tail.push(line);
                if tail.len() > LOG_TAIL {
                    tail.remove(0);
                }
            }
        }

        let status = child.wait().context("waiting for legendary install")?;
        if control.was_stopped() {
            return Ok(InstallEnd::Interrupted);
        }
        if !status.success() {
            bail!("legendary install failed:\n{}", tail.join("\n"));
        }
        Ok(InstallEnd::Done)
    }

    /// What the manifest says a game costs, whole and broken down by install
    /// tag. The manifest itself is several megabytes and legendary keeps no
    /// copy for a game that is not installed, so the numbers are cached under
    /// the build they describe: Epic mints a new build for every change, and
    /// the manifest for one never changes afterwards.
    pub fn manifest(&self, app_name: &str, version: &str) -> Result<ManifestInfo> {
        if let Some(cached) = read_cached_manifest(app_name, version) {
            return Ok(cached);
        }

        #[derive(serde::Deserialize)]
        struct TagSize {
            tag: String,
            size: u64,
        }
        #[derive(serde::Deserialize)]
        struct Manifest {
            #[serde(default)]
            download_size: u64,
            #[serde(default)]
            disk_size: u64,
            #[serde(default)]
            tag_download_size: Vec<TagSize>,
            #[serde(default)]
            tag_disk_size: Vec<TagSize>,
        }
        #[derive(serde::Deserialize)]
        struct Info {
            manifest: Option<Manifest>,
        }

        let info: Info = self.run_json(&["info", app_name, "--json"])?;
        let manifest = info
            .manifest
            .ok_or_else(|| anyhow!("no manifest for {app_name}"))?;
        let by_tag = |sizes: Vec<TagSize>| -> HashMap<String, u64> {
            sizes.into_iter().map(|s| (s.tag, s.size)).collect()
        };
        let info = ManifestInfo {
            download_size: manifest.download_size,
            disk_size: manifest.disk_size,
            tag_download: by_tag(manifest.tag_download_size),
            tag_disk: by_tag(manifest.tag_disk_size),
        };
        write_cached_manifest(app_name, version, &info);
        Ok(info)
    }

    /// legendary keeps a resume file for as long as a download is unfinished,
    /// which is what makes a job restored after a restart worth offering.
    pub fn can_resume(&self, app_name: &str) -> bool {
        self.config_dir
            .join("tmp")
            .join(format!("{app_name}.resume"))
            .is_file()
    }

    /// Everything legendary could carry on with, whether or not Divine still
    /// remembers asking for it.
    pub fn resumable(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(self.config_dir.join("tmp")) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                name.strip_suffix(".resume").map(str::to_string)
            })
            .collect()
    }

    /// The components legendary last installed a game with, straight from its
    /// config. A resume file on its own says nothing about them, and running
    /// without them would have legendary drop whatever they cover.
    pub fn stored_tags(&self, app_name: &str) -> Vec<String> {
        let Ok(text) = fs::read_to_string(self.config_dir.join("config.ini")) else {
            return Vec::new();
        };
        let mut inside = false;
        for line in text.lines().map(str::trim) {
            if let Some(section) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                inside = section == app_name;
            } else if inside {
                if let Some(value) = line.strip_prefix("install_tags") {
                    let value = value.trim_start().trim_start_matches('=');
                    // An empty value is the untagged part of the game, which
                    // is a selection in itself rather than the absence of one.
                    return value.trim().split(',').map(str::to_string).collect();
                }
            }
        }
        Vec::new()
    }

    /// Drops the resume file for a download nobody is going to carry on with.
    /// Left in place it would make legendary skip its scan for missing files
    /// and report that there is nothing to download; see InstallMode::resumable.
    pub fn forget_resume(&self, app_name: &str) {
        let _ = fs::remove_file(
            self.config_dir
                .join("tmp")
                .join(format!("{app_name}.resume")),
        );
    }

    /// What an interrupted download was actually fetching, read back from the
    /// files it finished: legendary records each one in the resume file, and
    /// the manifest says which component every file belongs to. The config
    /// alone is not enough — a run that carried on without the components
    /// overwrites it with what it did instead.
    pub fn resumed_tags(&self, app_name: &str) -> Result<Vec<String>> {
        let resume = fs::read_to_string(
            self.config_dir
                .join("tmp")
                .join(format!("{app_name}.resume")),
        )
        .context("reading the resume file")?;
        let done: Vec<&str> = resume
            .lines()
            .filter_map(|line| line.trim_end().split_once(':'))
            .map(|(_, filename)| filename)
            .collect();

        let files = self.list_files(app_name)?;
        let mut tags = self.stored_tags(app_name);
        for file in &files {
            if done.contains(&file.filename.as_str()) {
                tags.extend(file.install_tags.iter().cloned());
            }
        }
        // The untagged part of a game is always wanted.
        tags.push(String::new());
        tags.sort();
        tags.dedup();
        Ok(tags)
    }

    /// Every file a game's manifest ships, as paths inside its install folder.
    /// Needs the network for a game that is not installed: legendary fetches
    /// the manifest to answer.
    pub fn files(&self, app_name: &str) -> Result<Vec<String>> {
        Ok(self
            .list_files(app_name)?
            .into_iter()
            .map(|file| file.filename)
            .collect())
    }

    fn list_files(&self, app_name: &str) -> Result<Vec<FileEntry>> {
        self.run_json(&["list-files", app_name, "--json"])
            .with_context(|| format!("listing the files of {app_name}"))
    }

    /// The title legendary last saw for a game, without touching the network.
    pub fn cached_title(&self, app_name: &str) -> Option<String> {
        let path = self
            .config_dir
            .join("metadata")
            .join(format!("{app_name}.json"));
        let text = fs::read_to_string(path).ok()?;
        let json: serde_json::Value = serde_json::from_str(&text).ok()?;
        json.get("app_title")?.as_str().map(str::to_string)
    }

    /// Epic's overlay is downloaded once and shared, and legendary records that
    /// install in its own state directory.
    pub fn eos_overlay_installed(&self) -> bool {
        self.config_dir.join("overlay_install.json").is_file()
    }

    pub fn eos_overlay_install(&self, path: &Path) -> Result<()> {
        let path = path.display().to_string();
        self.run(&["eos-overlay", "install", "--path", &path])
            .map(|_| ())
    }

    /// Switches the overlay on or off for one prefix; it lives in the prefix's
    /// registry, so every game carries its own answer.
    pub fn eos_overlay_set(&self, prefix: &Path, enabled: bool) -> Result<()> {
        let action = if enabled { "enable" } else { "disable" };
        let prefix = prefix.display().to_string();
        self.run(&["eos-overlay", action, "--prefix", &prefix])
            .map(|_| ())
    }

    /// Checks every installed file against the manifest. `on_progress` gets the
    /// fraction done; the report says what, if anything, is broken.
    pub fn verify(&self, app_name: &str, on_progress: &dyn Fn(f64)) -> Result<VerifyReport> {
        let mut child = self
            .command(&["verify", app_name])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("running legendary verify")?;

        // Both pipes are drained at once: verify writes a line per bad file to
        // one and a progress line to the other, and a pipe left unread fills
        // up and stops the process that is writing to it.
        let stderr = child.stderr.take();
        let log = std::thread::spawn(move || {
            let mut report = VerifyReport::default();
            let mut tail: Vec<String> = Vec::new();
            let mut verdict = false;
            if let Some(stderr) = stderr {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    crate::log::from_tool("legendary", &line);
                    if line.contains("Verification finished successfully") {
                        verdict = true;
                    } else if let Some(counts) = verify_counts(&line) {
                        (report.corrupted, report.missing) = counts;
                        verdict = true;
                    }
                    tail.push(line);
                    if tail.len() > LOG_TAIL {
                        tail.remove(0);
                    }
                }
            }
            (report, verdict, tail)
        });

        // The progress line is rewritten in place with a carriage return, so it
        // never arrives as a "line" in the usual sense.
        if let Some(stdout) = child.stdout.take() {
            for chunk in split_on_returns(stdout) {
                if let Some(fraction) = verify_progress(&chunk) {
                    on_progress(fraction);
                }
            }
        }

        child.wait().context("waiting for legendary verify")?;
        let (report, verdict, tail) = log.join().map_err(|_| anyhow!("verify log thread died"))?;

        if !verdict {
            bail!("legendary verify said nothing useful:\n{}", tail.join("\n"));
        }
        Ok(report)
    }

    /// Syncs a game's saves with Epic. legendary works out where the game keeps
    /// them from the metadata and the prefix, which it reads from WINEPREFIX,
    /// and `--accept-path` keeps that first computation from asking.
    ///
    /// That computation needs the prefix's `user.reg`, because Epic writes the
    /// save folder as `{AppData}/../<game>` and only the registry says where
    /// `{AppData}` is. Without it legendary leaves the variable unexpanded,
    /// realpath collapses `{AppData}/..` to nothing, and `--accept-path` then
    /// accepts a *relative* path — saves land next to the working directory and
    /// get written into legendary's config as if that were correct. So the
    /// registry has to be there before any of this runs.
    pub fn sync_saves(&self, app_name: &str, prefix: &Path, mode: SaveSync) -> Result<Vec<String>> {
        if !prefix.join("user.reg").is_file() {
            bail!("the prefix is not set up yet — launch the game once, then sync");
        }

        let mut args = vec!["sync-saves", app_name, "--accept-path"];
        match mode {
            SaveSync::Both => {}
            SaveSync::Upload => args.push("--skip-download"),
            SaveSync::Download => args.push("--skip-upload"),
        }

        let out = self
            .command(&args)
            .env("WINEPREFIX", prefix)
            .output()
            .context("running legendary sync-saves")?;
        let log = String::from_utf8_lossy(&out.stderr);
        if !out.status.success() {
            bail!("legendary sync-saves failed:\n{}", tail_of(&log).join("\n"));
        }
        Ok(save_events(&log))
    }

    pub fn uninstall(&self, app_name: &str) -> Result<()> {
        self.run(&["uninstall", app_name]).map(|_| ())
    }

    /// Drops the record of an install and leaves its files alone, for a game
    /// whose folder cannot be reached. A drive plugged back in between the
    /// question and the answer must not have the game deleted off it.
    pub fn forget(&self, app_name: &str) -> Result<()> {
        self.run(&["uninstall", app_name, "--keep-files"])
            .map(|_| ())
    }

    pub fn import(&self, app_name: &str, path: &str) -> Result<()> {
        self.run(&["import", "--skip-dlcs", app_name, path])
            .map(|_| ())
    }

    /// Spawns the game and blocks until its output stream closes, i.e. until the
    /// game itself exits. legendary only spawns and returns, so it is reaped at
    /// the end; a short-lived zombie while the game runs is fine.
    ///
    /// `started` is handed the process group id as soon as there is one, which is
    /// what [`stop`] needs to reach the game.
    pub fn launch(
        &self,
        app_name: &str,
        wrapper: &str,
        env: &[(String, String)],
        extra_args: &[String],
        started: &dyn Fn(i32),
    ) -> LaunchOutcome {
        let mut cmd = self.command(&["launch", app_name, "--no-wine", "--wrapper", wrapper]);
        // legendary forwards anything after its own options to the game.
        cmd.args(extra_args);
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.stdout(Stdio::inherit()).stderr(Stdio::piped());
        // umu's container has no idea about a TMPDIR pointing outside of it,
        // which is what a nix develop shell hands us.
        cmd.env_remove("TMPDIR");
        cmd.env_remove("TMP");
        cmd.env_remove("TEMP");

        let mut inherited: BTreeMap<String, String> = std::env::vars().collect();
        inherited.extend(env.iter().cloned());
        let own: Vec<&str> = env.iter().map(|(name, _)| name.as_str()).collect();
        let left_out = over_budget(&inherited, &own);
        if !left_out.is_empty() {
            warn!(
                "the environment is too big for the game to inherit, leaving out {}",
                left_out.join(", ")
            );
            for name in &left_out {
                cmd.env_remove(name);
            }
        }

        // Its own process group, so stopping the game later can signal umu,
        // Proton and the game itself in one go rather than just legendary.
        cmd.process_group(0);

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                return LaunchOutcome {
                    ok: false,
                    log_tail: vec![format!("cannot run {}: {e}", self.bin)],
                }
            }
        };

        // The group id equals the leader's pid because of process_group(0).
        started(child.id() as i32);

        let mut tail: Vec<String> = Vec::new();
        if let Some(stderr) = child.stderr.take() {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                // Everything legendary runs writes here too, so this is the one
                // place that has to work out which of them is talking.
                crate::log::from_launch(&line);
                tail.push(line);
                if tail.len() > LOG_TAIL {
                    tail.remove(0);
                }
            }
        }
        let ok = child.wait().map(|s| s.success()).unwrap_or(false);
        LaunchOutcome { ok, log_tail: tail }
    }
}

/// What the environment of a game may weigh.
///
/// Wine stops being able to start processes at all once the environment it
/// inherits grows past about 40 KiB: the game's own launcher fails with
/// "ShellExecuteEx failed" and nothing else is said. Measured against
/// GE-Proton 11-5 and Grand Theft Auto V, which starts with 40 794 bytes of
/// environment and does not with 40 954. The budget stops well short of that,
/// since wine and umu both add to it underneath.
const ENV_BUDGET: usize = 32 * 1024;

/// Variables that stay whatever they weigh, because without them a game finds
/// neither its libraries nor the display it is meant to open on.
const ENV_ESSENTIAL: &[&str] = &[
    "PATH",
    "HOME",
    "LD_LIBRARY_PATH",
    "LD_PRELOAD",
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "XDG_RUNTIME_DIR",
    "XDG_DATA_DIRS",
];

/// The variables to leave out so that what the game inherits fits [`ENV_BUDGET`].
///
/// Divine has no way of knowing what a variable somebody set is for, so it only
/// steps in once the environment is already too big for wine, and then takes the
/// largest first: an environment that size belongs to a build shell, and the
/// kilobytes in it are compiler flags no game has ever read. `own` are the
/// variables Divine set for this launch, which are never candidates.
fn over_budget(env: &BTreeMap<String, String>, own: &[&str]) -> Vec<String> {
    // As the kernel counts it: "name=value" and the byte that ends it.
    let weight = |name: &str, value: &str| name.len() + value.len() + 2;
    let mut total: usize = env.iter().map(|(n, v)| weight(n, v)).sum();
    if total <= ENV_BUDGET {
        return Vec::new();
    }

    let mut droppable: Vec<(&String, &String)> = env
        .iter()
        .filter(|(name, _)| {
            !ENV_ESSENTIAL.contains(&name.as_str()) && !own.contains(&name.as_str())
        })
        .collect();
    droppable.sort_by_key(|(name, value)| std::cmp::Reverse(weight(name, value)));

    let mut left_out = Vec::new();
    for (name, value) in droppable {
        if total <= ENV_BUDGET {
            break;
        }
        total -= weight(name, value);
        left_out.push(name.clone());
    }
    left_out
}

/// Where a first install goes: legendary's own defaults for whatever is left
/// out, which are the install directory and the folder Epic names.
#[derive(Debug, Clone, Copy, Default)]
pub struct Place<'a> {
    pub base_path: Option<&'a str>,
    pub game_folder: Option<&'a str>,
}

/// One file out of `legendary list-files --json`.
#[derive(serde::Deserialize)]
struct FileEntry {
    filename: String,
    #[serde(default)]
    install_tags: Vec<String>,
}

/// The command line for one install, update, repair or component change.
fn install_args<'a>(
    app_name: &'a str,
    place: Place<'a>,
    mode: InstallMode,
    tags: &'a [String],
) -> Vec<&'a str> {
    let mut args: Vec<&str> = vec!["install", app_name];
    match mode {
        // --update-only alone is not enough. A game legendary has flagged for
        // verification makes it switch to repair mode on its own
        // (`cli.py`, `install_game`), and a repair without this flag does two
        // things an update must not: it targets the installed build rather than
        // the latest, and it sets `no_install`, which skips the call that writes
        // the new version down. The files land, nothing is recorded, and the
        // page offers the very same update again after a restart.
        InstallMode::Update => args.extend(["--update-only", "--repair-and-update"]),
        // Deliberately without --repair-and-update: this stays on the installed
        // build, which is what someone fixing a broken install expects.
        InstallMode::Repair => args.push("--repair"),
        // The tags carry the whole change; legendary works out what to
        // fetch and deletes the files behind whatever is no longer wanted.
        InstallMode::Components => {}
        InstallMode::Fresh { with_dlcs } => {
            if let Some(path) = place.base_path {
                args.extend(["--base-path", path]);
            }
            // Only a first install reads it: once a game is installed,
            // legendary keeps the folder it already has (`prepare_download`).
            if let Some(folder) = place.game_folder {
                args.extend(["--game-folder", folder]);
            }
            // Neither prompt can be answered from here, so the answer is
            // always given up front.
            args.push(if with_dlcs {
                "--with-dlcs"
            } else {
                "--skip-dlcs"
            });
        }
    }
    // Without a choice of components, take the default set. legendary only
    // knows a couple of games itself, so anything picked here is passed on
    // explicitly; it remembers the list for later updates.
    if tags.is_empty() {
        args.push("--skip-sdl");
    } else {
        for tag in tags {
            args.extend(["--install-tag", tag]);
        }
    }
    args
}

/// Folds one status line into the running progress. Returns true when the line
/// carried a new measurement worth showing.
fn update(progress: &mut Progress, line: &str) -> bool {
    if let Some(total) = number_after(line, "Download size:") {
        progress.total_mib = total;
        return true;
    }
    if let Some(speed) = number_after(line, "+ Download\t-") {
        progress.speed_mib = speed;
        return false;
    }
    if let Some(eta) = line.split("ETA:").nth(1) {
        progress.eta = eta.trim().to_string();
        return false;
    }
    let Some(done) = number_after(line, "- Downloaded:") else {
        return false;
    };
    progress.done_mib = done;
    if progress.total_mib > 0.0 {
        progress.fraction = (done / progress.total_mib).clamp(0.0, 1.0);
    }
    true
}

/// Pulls the number that follows `marker`, e.g. "Download size: 512.00 MiB".
fn number_after(line: &str, marker: &str) -> Option<f64> {
    line.split(marker)
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// Splits a stream on carriage returns as well as newlines, which is how a
/// progress line that rewrites itself has to be read.
fn split_on_returns(stream: impl std::io::Read) -> impl Iterator<Item = String> {
    let mut reader = BufReader::new(stream);
    std::iter::from_fn(move || loop {
        let mut chunk = Vec::new();
        let mut ended = false;
        loop {
            let mut byte = [0u8; 1];
            match reader.read(&mut byte) {
                Ok(0) => {
                    ended = true;
                    break;
                }
                Ok(_) => {
                    if byte[0] == b'\r' || byte[0] == b'\n' {
                        break;
                    }
                    chunk.push(byte[0]);
                }
                Err(_) => {
                    ended = true;
                    break;
                }
            }
        }
        // Only the end of the stream ends the iteration. Two delimiters in a
        // row give an empty chunk, and stopping on that would close the pipe
        // under a process still writing to it — which legendary does not
        // survive: its next flush dies with a broken pipe.
        if !chunk.is_empty() {
            return Some(String::from_utf8_lossy(&chunk).into_owned());
        }
        if ended {
            return None;
        }
    })
}

/// "Verification progress: 120/3000 (4.0%) [12.3 MiB/s]" -> 0.04
fn verify_progress(chunk: &str) -> Option<f64> {
    let percent = chunk.split('(').nth(1)?.split('%').next()?;
    Some((percent.trim().parse::<f64>().ok()? / 100.0).clamp(0.0, 1.0))
}

/// "Verification failed, 2 file(s) corrupted, 1 file(s) are missing."
fn verify_counts(line: &str) -> Option<(usize, usize)> {
    let rest = line.split("Verification failed,").nth(1)?;
    let numbers: Vec<usize> = rest
        .split_whitespace()
        .filter_map(|word| word.parse().ok())
        .collect();
    Some((*numbers.first()?, *numbers.get(1)?))
}

/// The lines from a save sync that are worth repeating to the user.
fn save_events(log: &str) -> Vec<String> {
    const INTERESTING: [&str; 4] = [
        "Downloading remote savegame",
        "Uploading savegame",
        "is up to date",
        "No cloud or local savegame",
    ];
    log.lines()
        .filter(|line| INTERESTING.iter().any(|needle| line.contains(needle)))
        .map(|line| {
            line.split("INFO: ")
                .last()
                .unwrap_or(line)
                .trim()
                .to_string()
        })
        .collect()
}

/// Signals a launched game's whole process group. SIGTERM lets Proton shut the
/// prefix down cleanly; SIGKILL is for when it has stopped responding.
pub fn stop(process_group: i32, force: bool) {
    let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
    // Negative pid means "the whole group", which is why launch created one.
    unsafe { libc::kill(-process_group, signal) };
}

/// Games installed through a system-wide legendary or through Heroic. Divine has
/// its own legendary state, so these have to be imported before they can be run.
pub fn foreign_installs() -> Vec<InstalledEntry> {
    let mut found: Vec<InstalledEntry> = Vec::new();
    for dir in config::foreign_legendary_dirs() {
        let installed: HashMap<String, InstalledEntry> =
            match read_json(&dir.join("installed.json")) {
                Some(map) => map,
                None => continue,
            };
        for entry in installed.into_values() {
            let known = found.iter().any(|e| e.app_name == entry.app_name);
            if !known && Path::new(&entry.install_path).is_dir() {
                found.push(entry);
            }
        }
    }
    found
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

/// Strips legendary's `[source] LEVEL: ` prefix off a log line.
fn message(line: &str) -> &str {
    match line.find(']') {
        Some(bracket) => line[bracket + 1..]
            .split_once(": ")
            .map_or(line, |(_, rest)| rest)
            .trim(),
        None => line.trim(),
    }
}

/// Reads the outcome of `activate --uplay` out of what legendary logged.
fn read_activation(log: &str) -> Activation {
    let mut titles = Vec::new();
    let mut failed = Vec::new();
    // The titles come as a bullet list right after the count, and stop at the
    // first line that is not one.
    let mut listing = false;

    for line in log.lines() {
        let line = message(line);
        if line.contains("No linked ubisoft account found") {
            return Activation::NotLinked;
        }
        if line.contains("have already been activated") {
            return Activation::NothingLeft;
        }
        if line.starts_with("Found ") && line.contains("to redeem") {
            listing = true;
            continue;
        }
        if listing {
            match line.strip_prefix("- ") {
                Some(title) => {
                    titles.push(title.to_string());
                    continue;
                }
                None => listing = false,
            }
        }
        if let Some(rest) = line.strip_prefix("Claiming \"") {
            if let Some((title, _)) = rest.split_once("\" failed") {
                failed.push(title.to_string());
            }
        }
    }

    titles.retain(|title| !failed.contains(title));
    Activation::Claimed { titles, failed }
}

fn tail_of(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines
        .iter()
        .skip(lines.len().saturating_sub(LOG_TAIL))
        .map(|l| l.to_string())
        .collect()
}

/// Cached manifest sizes live under the build they were read from, so a game
/// that Epic has since updated falls through to a fresh look.
pub fn manifest_cache_file(app_name: &str, version: &str) -> Option<PathBuf> {
    if version.is_empty() {
        return None;
    }
    let safe: String = version
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    Some(crate::config::manifests_dir().join(format!("{app_name}-{safe}.json")))
}

fn read_cached_manifest(app_name: &str, version: &str) -> Option<ManifestInfo> {
    let text = fs::read_to_string(manifest_cache_file(app_name, version)?).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_cached_manifest(app_name: &str, version: &str, info: &ManifestInfo) {
    let Some(path) = manifest_cache_file(app_name, version) else {
        return;
    };
    let Some(dir) = path.parent() else {
        return;
    };
    if fs::create_dir_all(dir).is_err() {
        return;
    }
    // One file per game: the build that just arrived replaces the older ones.
    if let Ok(entries) = fs::read_dir(dir) {
        let prefix = format!("{app_name}-");
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(&prefix) && entry.path() != path {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    if let Ok(text) = serde_json::to_string(info) {
        let _ = fs::write(path, text);
    }
}

#[cfg(test)]
mod tests {
    use super::{install_args, InstallMode, Place};

    /// The bug this guards: Grand Theft Auto V sat flagged for verification, so
    /// legendary quietly turned the update into a repair, fetched the files and
    /// never wrote the new version down. The page offered the same update after
    /// every restart, and taking it repaired the old build again.
    #[test]
    fn an_update_still_finishes_when_legendary_drops_into_repair_mode() {
        let args = install_args("GameName", Place::default(), InstallMode::Update, &[]);
        assert!(args.contains(&"--update-only"));
        assert!(args.contains(&"--repair-and-update"));
    }

    #[test]
    fn a_repair_stays_on_the_build_that_is_installed() {
        let args = install_args("GameName", Place::default(), InstallMode::Repair, &[]);
        assert!(args.contains(&"--repair"));
        assert!(!args.contains(&"--repair-and-update"));
    }

    /// A folder name with spaces in it is one argument, not three.
    #[test]
    fn a_first_install_lands_in_the_folder_it_was_given() {
        let args = install_args(
            "8769e24080ea413b8ebca3f1b8c50951",
            Place {
                base_path: Some("/Games"),
                game_folder: Some("Grand Theft Auto V Enhanced"),
            },
            InstallMode::Fresh { with_dlcs: false },
            &[],
        );
        let at = args
            .iter()
            .position(|arg| *arg == "--game-folder")
            .expect("a --game-folder argument");
        assert_eq!(args[at + 1], "Grand Theft Auto V Enhanced");
    }

    #[test]
    fn chosen_components_replace_the_default_set() {
        let tags = ["ru".to_string(), "textures".to_string()];
        let args = install_args("GameName", Place::default(), InstallMode::Components, &tags);
        assert!(!args.contains(&"--skip-sdl"));
        assert_eq!(
            args,
            [
                "install",
                "GameName",
                "--install-tag",
                "ru",
                "--install-tag",
                "textures"
            ]
        );
    }

    #[test]
    fn an_empty_chunk_is_not_the_end_of_the_stream() {
        let stream = "first\r\nsecond\r\r third\n".as_bytes();
        let chunks: Vec<String> = super::split_on_returns(stream).collect();
        assert_eq!(chunks, ["first", "second", " third"]);
    }

    use super::*;

    #[test]
    fn an_ordinary_environment_is_handed_over_whole() {
        let env: BTreeMap<String, String> = [
            ("PATH", "/run/current-system/sw/bin:/usr/bin"),
            ("HOME", "/home/nedo"),
            ("DXVK_HUD", "fps"),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
        assert!(over_budget(&env, &["GAMEID"]).is_empty());
    }

    #[test]
    fn a_build_shell_loses_its_compiler_flags() {
        // The sizes are the ones a nix develop shell for this project really
        // has, and together they are what stops the game from starting.
        let mut env: BTreeMap<String, String> = [
            ("NIX_CFLAGS_COMPILE", 6579),
            ("NIX_LDFLAGS", 5172),
            ("PATH", 4778),
            ("PKG_CONFIG_PATH", 3191),
            ("LS_COLORS", 1906),
            ("XDG_DATA_DIRS", 1093),
        ]
        .into_iter()
        .map(|(name, size)| (name.to_string(), "x".repeat(size)))
        .collect();
        for i in 0..20 {
            env.insert(format!("REST_{i}"), "x".repeat(1000));
        }
        env.insert("GAMEID".into(), "umu-0".into());

        let left_out = over_budget(&env, &["GAMEID"]);
        assert_eq!(left_out, ["NIX_CFLAGS_COMPILE", "NIX_LDFLAGS"]);
    }

    #[test]
    fn what_a_game_cannot_start_without_is_never_left_out() {
        let mut env: BTreeMap<String, String> = BTreeMap::new();
        // Absurd on purpose: even the largest variable in the environment stays
        // when a game would be lost without it.
        env.insert("PATH".into(), "x".repeat(40 * 1024));
        env.insert("GAMEID".into(), "x".repeat(4096));
        env.insert("LESS_TERMCAP".into(), "x".repeat(4096));

        let left_out = over_budget(&env, &["GAMEID"]);
        assert_eq!(left_out, ["LESS_TERMCAP"]);
    }

    #[test]
    fn an_unlinked_ubisoft_account_is_recognised() {
        let log = "[cli] ERROR: No linked ubisoft account found! Link your accounts \
                   via your browser and try again.";
        assert_eq!(read_activation(log), Activation::NotLinked);
    }

    #[test]
    fn a_library_with_nothing_left_says_so() {
        let log = "[cli] INFO: All of your 2 titles have already been activated \
                   on your Ubisoft account.";
        assert_eq!(read_activation(log), Activation::NothingLeft);
    }

    /// Lines exactly as legendary writes them, down to the space before the
    /// dash in its bullet list.
    #[test]
    fn claimed_titles_are_read_off_the_log() {
        let log = "[cli] INFO: Found 2 game(s) to redeem:\n\
                   [cli] INFO:  - Trackmania Starter Access\n\
                   [cli] INFO:  - Watch Dogs 2\n\
                   [cli] INFO: Redeemed all outstanding Uplay codes.";
        assert_eq!(
            read_activation(log),
            Activation::Claimed {
                titles: vec!["Trackmania Starter Access".into(), "Watch Dogs 2".into()],
                failed: vec![],
            }
        );
    }

    #[test]
    fn a_title_that_would_not_claim_is_not_counted_as_claimed() {
        let log = "[cli] INFO: Found 2 game(s) to redeem:\n\
                   [cli] INFO:  - Trackmania Starter Access\n\
                   [cli] INFO:  - Watch Dogs 2\n\
                   [cli] ERROR: Claiming \"Watch Dogs 2\" failed with: Key already used";
        assert_eq!(
            read_activation(log),
            Activation::Claimed {
                titles: vec!["Trackmania Starter Access".into()],
                failed: vec!["Watch Dogs 2".into()],
            }
        );
    }

    /// Lines exactly as legendary writes them, taken from its own format strings.
    #[test]
    fn progress_follows_bytes_not_chunk_count() {
        let mut progress = Progress::default();

        assert!(update(
            &mut progress,
            "[DLManager] INFO: Download size: 500.00 MiB (Compression savings: 12.3%)"
        ));
        assert_eq!(progress.total_mib, 500.0);

        update(
            &mut progress,
            "[DLManager] INFO:  + Download\t- 25.50 MiB/s (raw) / 30.00 MiB/s (decompressed)",
        );
        update(
            &mut progress,
            "[DLManager] INFO: = Progress: 99.20% (7440/7500), Running for 00:00:12, ETA: 00:00:04",
        );

        // The chunk based percentage above is nearly complete because an update
        // reuses local chunks; the bytes actually fetched tell the real story.
        assert!(update(
            &mut progress,
            "[DLManager] INFO:  - Downloaded: 125.00 MiB, Written: 480.00 MiB"
        ));
        assert_eq!(progress.fraction, 0.25);
        assert_eq!(progress.speed_mib, 25.5);
        assert_eq!(progress.eta, "00:00:04");
    }

    #[test]
    fn progress_stays_sane_without_a_total() {
        let mut progress = Progress::default();
        assert!(update(
            &mut progress,
            " - Downloaded: 10.00 MiB, Written: 10.00 MiB"
        ));
        assert_eq!(progress.fraction, 0.0);
    }
}
