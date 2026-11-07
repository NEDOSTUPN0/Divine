//! Launch options written the way Steam takes them: `gamemoderun %command% -w 1280`.
//!
//! Steam hands the line to a shell. Divine has none in between, so the line is
//! split into words the way a shell would split it and nothing more: quotes and
//! backslashes work, while `$VAR`, `~` and globs stay exactly as typed. Words
//! before `%command%` wrap the launch, words after it go to the game, and
//! `KEY=value` words at the very start set the environment, as they would at a
//! prompt. A line without `%command%` is nothing but game arguments, which is
//! how Steam reads one too, and how the field read before it took wrappers.

use anyhow::{bail, Result};

/// Where Divine's own command goes.
pub const COMMAND: &str = "%command%";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct LaunchOptions {
    pub env: Vec<(String, String)>,
    /// Programs the whole launch runs under, outermost first.
    pub wrapper: Vec<String>,
    pub args: Vec<String>,
}

pub fn parse(line: &str) -> Result<LaunchOptions> {
    let mut words = split(line)?;
    let Some(at) = words.iter().position(|word| word == COMMAND) else {
        return Ok(LaunchOptions {
            args: words,
            ..LaunchOptions::default()
        });
    };

    let args = words.split_off(at + 1);
    if args.iter().any(|word| word == COMMAND) {
        bail!("{COMMAND} can only appear once");
    }
    words.pop();

    let assignments = words
        .iter()
        .take_while(|word| assignment(word).is_some())
        .count();
    let env = words
        .drain(..assignments)
        .filter_map(|word| assignment(&word))
        .collect();

    Ok(LaunchOptions {
        env,
        wrapper: words,
        args,
    })
}

/// `KEY=value`, when the key is a name a shell would accept as a variable.
fn assignment(word: &str) -> Option<(String, String)> {
    let (key, value) = word.split_once('=')?;
    let mut chars = key.chars();
    let leads = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    (leads && chars.all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .then(|| (key.to_string(), value.to_string()))
}

/// Word splitting as POSIX sh does it, without any of the expansions.
fn split(line: &str) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    // Separate from `word` being empty: `''` is a word, and an empty one.
    let mut in_word = false;
    let mut chars = line.chars();

    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => word.push(c),
                        None => bail!("a single quote is never closed"),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        // Inside double quotes a backslash only escapes the
                        // characters that would mean something there.
                        Some('\\') => match chars.next() {
                            Some(c @ ('"' | '\\' | '$' | '`')) => word.push(c),
                            Some(c) => {
                                word.push('\\');
                                word.push(c);
                            }
                            None => bail!("a double quote is never closed"),
                        },
                        Some(c) => word.push(c),
                        None => bail!("a double quote is never closed"),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next() {
                    Some(c) => word.push(c),
                    None => bail!("the line ends in a backslash with nothing to escape"),
                }
            }
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|word| word.to_string()).collect()
    }

    #[test]
    fn a_wrapper_goes_before_the_command_and_arguments_after_it() {
        let options = parse("gamemoderun %command% -w 1080 -h 720").expect("parsing");
        assert_eq!(options.wrapper, words(&["gamemoderun"]));
        assert_eq!(options.args, words(&["-w", "1080", "-h", "720"]));
        assert!(options.env.is_empty());
    }

    /// What the field held before it understood `%command%` must mean the
    /// same thing now, or every game set up back then changes how it starts.
    #[test]
    fn a_line_without_the_command_is_all_game_arguments() {
        let options = parse("-dx11 -windowed").expect("parsing");
        assert_eq!(options.args, words(&["-dx11", "-windowed"]));
        assert!(options.wrapper.is_empty());

        // Even an assignment: with nothing to wrap, it is the game's to read.
        assert_eq!(parse("FOO=1").expect("parsing").args, words(&["FOO=1"]));
        assert_eq!(parse("  ").expect("parsing"), LaunchOptions::default());
    }

    #[test]
    fn leading_assignments_become_the_environment() {
        let options =
            parse("DXVK_HUD=fps PROTON_LOG=1 mangohud --dlsym %command%").expect("parsing");
        assert_eq!(
            options.env,
            [
                ("DXVK_HUD".to_string(), "fps".to_string()),
                ("PROTON_LOG".to_string(), "1".to_string()),
            ]
        );
        assert_eq!(options.wrapper, words(&["mangohud", "--dlsym"]));
        assert!(options.args.is_empty());

        // Past the first program they are its arguments, as in a shell, so
        // `env` keeps working the way it always does.
        let options = parse("env A=1 %command%").expect("parsing");
        assert!(options.env.is_empty());
        assert_eq!(options.wrapper, words(&["env", "A=1"]));
    }

    #[test]
    fn quotes_keep_spaces_inside_one_word() {
        let options = parse(
            r#"WINEDLLOVERRIDES="dinput8=n,b" %command% -name 'Tom Smith' a\ b "say \"hi\"""#,
        )
        .expect("parsing");
        assert_eq!(
            options.env,
            [("WINEDLLOVERRIDES".to_string(), "dinput8=n,b".to_string())]
        );
        assert_eq!(
            options.args,
            words(&["-name", "Tom Smith", "a b", r#"say "hi""#])
        );
        assert_eq!(
            parse("%command% '' -x").expect("parsing").args,
            words(&["", "-x"])
        );
    }

    /// Nothing expands: there is no shell to do it, and a half-done imitation
    /// would surprise more than none.
    #[test]
    fn variables_and_the_home_directory_are_left_as_typed() {
        let options = parse(r#"%command% $HOME ~/saves "\$X""#).expect("parsing");
        assert_eq!(options.args, words(&["$HOME", "~/saves", "$X"]));
    }

    #[test]
    fn a_line_a_shell_would_reject_is_refused() {
        assert!(parse("%command% -name 'Tom").is_err());
        assert!(parse(r#"%command% "half"#).is_err());
        assert!(parse(r"%command% \").is_err());
        assert!(parse("%command% %command%").is_err());
    }
}
