# Epic's playtime service

Epic counts how long you have played each game and shows it in the Windows
launcher. The service behind it is not documented anywhere Epic publishes, so
everything here was read off the wire of the real launcher rather than guessed.
Divine sends exactly what Windows sends.

Captured on 2026-08-19 from **Epic Games Launcher 20.2.1** (`EpicInstaller-20.1.4.msi`,
self-updated on first run) running under Proton.

## The endpoints

All four live under `https://library-service.live.use1a.on.epicgames.com/library/api/public`
and are named verbatim in the launcher binary, inside `OnlineSubsystemMcp.OnlineLibraryServiceMcp`:

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/playtime/account/{accountId}/all` | every game's total |
| `GET` | `/playtime/account/{accountId}/artifact/{artifactId}` | one game's total |
| `PUT` | `/playtime/account/{accountId}` | report one session |
| `PUT` | `/playtime/account/{accountId}/bulk` | report several |

Authentication is the ordinary launcher OAuth token — the one legendary keeps in
`user.json` — as `Authorization: <token_type> <access_token>`.

`artifactId` is what legendary calls `app_name`. Verified against a whole
library: 14 of 14 matched, whether the name was a word like `Fowl` or a hex
blob like `5b60142e120c4f2d88027595c21d4a04`.

## Reading

```
GET /library/api/public/playtime/account/{accountId}/all
```

```json
[ { "accountId": "…", "artifactId": "Fowl", "totalTime": 13169 } ]
```

`totalTime` is seconds.

## Reporting

The launcher sends **one record when the game exits**, covering the whole
session, and uses `/bulk` even for a single record. Verified with an eleven
minute session: it arrived as one record, not as segments.

```
PUT /library/api/public/playtime/account/{accountId}/bulk        -> 204
User-Agent: UELauncher/20.2.1-56875188+++UE5+Release-Distro-5.5 Windows/10.0.19045.1.0.64bit
Content-Type: application/json
```

```json
{
  "playtimeAddList": [
    {
      "machineId": "ebf4e8a6-8c03-4b3e-9382-6de0a7ca8c57",
      "artifactId": "5b60142e120c4f2d88027595c21d4a04",
      "startTime": "2026-08-19T00:54:20.173Z",
      "endTime": "2026-08-19T01:05:23.602Z",
      "startSegment": true,
      "endSegment": true
    }
  ]
}
```

* `machineId` is a UUIDv4, one per installation.
* Timestamps are ISO-8601 UTC with milliseconds. `startTime` is the moment the
  game launched — in the capture it followed the pre-launch save download by two
  seconds, and the report went out in the same burst as the post-exit save upload.
* A game that crashed produced no request at all.

### The journal

While a game runs the launcher writes the session to disk about every three
minutes; the binary carries a log category called `LogPlaytimeTrackingCache` for
exactly this. A clean exit sends the final record and clears the journal. If the
launcher dies first, the stale entry is flushed on a later report with
`endSegment: false` — "this segment is not known to have ended".

This was caught by accident. Killing the launcher after a session left an entry
behind, and the next session's report carried both:

```json
{"playtimeAddList": [
  {"startTime": "…00:42:04.532Z", "endTime": "…00:44:56.714Z",
   "startSegment": true, "endSegment": false},
  {"startTime": "…00:54:20.173Z", "endTime": "…01:05:23.602Z",
   "startSegment": true, "endSegment": true}
]}
```

### Deduplication

The server keys on `(machineId, artifactId, startTime)` and keeps the longest
span it has seen. Measured against the two reports above, where the first
record overlapped a session already counted:

```
213 s before  +  180 s (session)  +  172 s (resent overlap)  +  663 s (session)
naive sum: 1228        Epic reports: 1056 = 213 + 180 + 663
```

The overlapping record added nothing. **Reporting the same session twice cannot
inflate the total**, which is what makes a dumb retry queue safe.

## Two traps

Both of these would have produced a wrong implementation, and both were only
caught by capturing real traffic:

* The launcher binary contains the string `{"playtimeList":`, which looks like
  the bulk request key. It is not — it belongs to response parsing. The wire
  uses `playtimeAddList`, as the older community documentation says.
* The strings `startSegment` and `endSegment` do **not** appear anywhere in the
  binary, in any encoding. They are still sent on every request.

## Reproducing the capture

`mitmproxy` in front of the launcher, with the launcher running under Proton:

1. `mitmdump --set confdir=<dir> -p 8080`
2. Append `<dir>/mitmproxy-ca-cert.pem` to the launcher's own CA bundle at
   `Engine/Content/Certificates/ThirdParty/cacert.pem`. Unreal's HTTP goes
   through libcurl with that bundle, not the Windows certificate store. Note
   that a launcher self-update replaces the file and the CA has to go back in.
3. Point it at the proxy through
   `…/EpicGamesLauncher/Saved/Config/WindowsEditor/Engine.ini` — `WindowsEditor`,
   not `Windows`:

   ```ini
   [HTTP.Curl]
   bUseHttpProxy=true
   HttpProxyAddress=127.0.0.1:8080
   ```

   `bVerifyPeer=false` in the same section is read but ignored, so trusting the
   CA is the part that matters. `-httpproxy=127.0.0.1:8080` on the command line
   works too, but a self-update relaunches without it.

The launcher prints what it settled on at startup, which is the quickest way to
tell whether the configuration took:

```
LogInit:  CurlRequestOptions (configurable via config and command line):
LogInit:  - bVerifyPeer = true  - Libcurl will verify peer certificate
LogInit:  - bUseHttpProxy = true  - Libcurl will use HTTP proxy
LogInit:  - HttpProxyAddress = '127.0.0.1:8080'
```

## What Divine does with it

`src/playtime.rs`. Sessions are journalled the same way, with the same three
minute checkpoint, and reported as a single record when the game exits.
Reporting is off by default — it is not a published API — and lives behind
**Report playtime to Epic** in the preferences. Divine records playtime either
way; the switch only decides whether it leaves the machine.

Only sessions that ended within the last day are ever reported. Reporting can
be switched on long after the journal started filling up, and handing Epic
months of history in one go would look like anything but playing games; older
sessions stay on the machine and still count towards the hours Divine shows.
Switching reporting on while such sessions exist says so, because from then on
the two counters disagree by exactly that much.

However many games a report covers, it is one request — the `/bulk` route
exists for that — so a backlog cannot become a burst of traffic.
