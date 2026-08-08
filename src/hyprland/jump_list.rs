use std::{fs, path::Path, process::Command, thread};

use log::{debug, warn};

use super::icons::data_directories;

/// A Desktop Entry action supplied by the active application's package.
///
/// Desktop Entry `Actions` are the portable Linux equivalent of a jump list.
/// They are deliberately used here instead of trying to inspect an application's
/// private menu over Wayland, which has no compositor-independent protocol.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JumpListAction {
    pub label: String,
    desktop_id: String,
    action_id: String,
    fallback_exec: String,
}

impl JumpListAction {
    pub fn resolve(app_id: &str, initial_app_id: &str) -> Vec<Self> {
        let candidates = application_ids(app_id, initial_app_id);
        if candidates.is_empty() {
            return Vec::new();
        }

        for data_dir in data_directories() {
            let applications = data_dir.join("applications");
            let Ok(entries) = fs::read_dir(applications) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|extension| extension.to_str()) != Some("desktop") {
                    continue;
                }
                if let Some(actions) = actions_in_entry(&path, &candidates) {
                    return actions;
                }
            }
        }
        Vec::new()
    }
}

/// Starts a jump-list action outside the UI thread.
pub fn launch(action: JumpListAction) {
    thread::Builder::new()
        .name("bah-jump-list-action".to_string())
        .spawn(move || launch_inner(action))
        .map_err(|error| warn!("failed to start jump-list action: {error}"))
        .ok();
}

fn launch_inner(action: JumpListAction) {
    let Some(argv) = parse_exec(&action.fallback_exec) else {
        warn!(
            "jump-list action {} in {} has no runnable Exec command",
            action.action_id, action.desktop_id
        );
        return;
    };
    let (program, arguments) = argv.split_first().expect("non-empty command");
    debug!(
        "launching Desktop Action {} from {}",
        action.action_id, action.desktop_id
    );
    if let Err(error) = Command::new(program).args(arguments).spawn() {
        warn!(
            "failed to launch jump-list action {} for {}: {error}",
            action.action_id, action.desktop_id
        );
    }
}

fn application_ids(app_id: &str, initial_app_id: &str) -> Vec<String> {
    [app_id, initial_app_id]
        .into_iter()
        .map(|value| {
            value
                .split_once('\0')
                .map_or(value, |(value, _)| value)
                .trim()
        })
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn actions_in_entry(path: &Path, candidates: &[String]) -> Option<Vec<JumpListAction>> {
    let contents = fs::read_to_string(path).ok()?;
    let desktop_id = path.file_stem()?.to_str()?.to_string();
    let entry = section(&contents, "Desktop Entry")?;
    let file_stem_matches = candidates
        .iter()
        .any(|candidate| candidate == &desktop_id.to_lowercase());
    let wm_class_matches = entry.get("StartupWMClass").is_some_and(|class| {
        candidates
            .iter()
            .any(|candidate| candidate == &class.to_lowercase())
    });
    if !file_stem_matches && !wm_class_matches {
        return None;
    }

    let action_ids = entry.get("Actions")?.split(';').filter(|id| !id.is_empty());
    let mut actions = Vec::new();
    for action_id in action_ids {
        let action = section(&contents, &format!("Desktop Action {action_id}"))?;
        let Some(fallback_exec) = action.get("Exec") else {
            continue;
        };
        let label = action
            .get("Name")
            .cloned()
            .or_else(|| {
                action
                    .iter()
                    .find_map(|(key, value)| key.starts_with("Name[").then(|| value.clone()))
            })
            .unwrap_or_else(|| action_id.to_string());
        actions.push(JumpListAction {
            label,
            desktop_id: desktop_id.clone(),
            action_id: action_id.to_string(),
            fallback_exec: fallback_exec.clone(),
        });
    }
    Some(actions)
}

fn section(contents: &str, wanted: &str) -> Option<std::collections::HashMap<String, String>> {
    let mut current = None;
    let mut values = std::collections::HashMap::new();
    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            current = Some(&line[1..line.len() - 1] == wanted);
            continue;
        }
        if current != Some(true) || line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            values.insert(key.to_string(), value.to_string());
        }
    }
    (!values.is_empty()).then_some(values)
}

/// Parse the limited shell-like quoting used by Desktop Entry Exec values. Field
/// codes are removed because a jump-list action has no file/URL argument.
fn parse_exec(exec: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for character in exec.chars() {
        if escaped {
            word.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '"' {
            quoted = !quoted;
        } else if character.is_whitespace() && !quoted {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        } else {
            word.push(character);
        }
    }
    if escaped || quoted {
        return None;
    }
    if !word.is_empty() {
        words.push(word);
    }

    let words = words
        .into_iter()
        .map(|word| remove_field_codes(&word))
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    (!words.is_empty()).then_some(words)
}

fn remove_field_codes(value: &str) -> String {
    let mut result = String::new();
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character != '%' {
            result.push(character);
            continue;
        }
        match characters.next() {
            Some('%') => result.push('%'),
            // A jump-list action is not passed a file, URI, icon, or desktop
            // path, so these standard Desktop Entry placeholders disappear.
            Some('f' | 'F' | 'u' | 'U' | 'i' | 'c' | 'k') => {}
            Some(other) => {
                result.push('%');
                result.push(other);
            }
            None => result.push('%'),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{parse_exec, remove_field_codes};

    #[test]
    fn exec_parser_handles_quotes_and_field_codes() {
        assert_eq!(
            parse_exec("browser --new-window %U --label=\"New Window\""),
            Some(vec![
                "browser".to_string(),
                "--new-window".to_string(),
                "--label=New Window".to_string(),
            ])
        );
        assert_eq!(remove_field_codes("%%-%c-%f"), "%--");
    }
}
