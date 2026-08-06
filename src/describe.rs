use anyhow::{Result, anyhow};
use clap::{Arg, Command};
use serde_json::{Map, Value, json};

pub fn generate(mut root: Command, path: &[String]) -> Result<Value> {
    root.build();

    let mut command = &root;
    let mut resolved_path = Vec::new();
    for name in path {
        let next = command.get_subcommands().find(|subcommand| {
            subcommand.get_name() == name
                && !subcommand.is_hide_set()
                && subcommand.get_name() != "help"
        });
        let Some(next) = next else {
            return Err(unknown_command(command, &resolved_path, name));
        };
        command = next;
        resolved_path.push(name.clone());
    }

    let global_arguments = root
        .get_arguments()
        .filter(|argument| visible_argument(argument) && argument.is_global_set())
        .map(|argument| describe_argument(&root, argument))
        .collect::<Vec<_>>();

    Ok(json!({
        "schemaVersion": 1,
        "cliVersion": root.get_version().unwrap_or(env!("CARGO_PKG_VERSION")),
        "command": describe_command(command, &resolved_path),
        "globalArguments": global_arguments,
        "outputContract": {
            "default": "Deterministic line-oriented text.",
            "json": "Pass --json to emit the complete API response as one JSON value for non-streaming commands.",
            "loginJson": "Login emits an authorize event followed by an authenticated event as NDJSON on stdout.",
            "eventWatchJson": "Event watches emit one JSON value per line as NDJSON.",
            "waitProgress": "Interactive waits report unhealthy-to-healthy transitions on stderr; progress is suppressed when stderr is redirected or --json is used.",
            "errors": "Errors are written to stderr and return a non-zero exit code; --json also makes errors JSON."
        },
        "guidance": [
            "Use --json for complete machine-readable API responses and errors; login and event watches are streamed as NDJSON.",
            "Pod-scoped commands require --pod, BRAINPOD_POD, or a configured default pod.",
            "Image builds prefer an existing Dockerfile, otherwise use Railpack, target the best architecture supported by the API (override with --platform), and push to the selected pod's private registry namespace.",
            "Blueprint installation and resource mutations update the mutable draft; run deploy separately when ready, optionally with --wait.",
            "Use blueprint get to inspect blueprint documentation, defaults, and its input schema before installation.",
            "Use resource URNs returned by resource commands when querying events.",
            "Use `brainpod describe resource <kind>` to inspect the resource schema fetched from the production OpenAPI document; the embedded document is used when it is unavailable."
        ]
    }))
}

fn unknown_command(command: &Command, path: &[String], name: &str) -> anyhow::Error {
    let available = visible_subcommands(command)
        .map(Command::get_name)
        .collect::<Vec<_>>();
    let parent = if path.is_empty() {
        "brainpod".to_owned()
    } else {
        format!("brainpod {}", path.join(" "))
    };

    if available.is_empty() {
        anyhow!("`{parent}` has no subcommand `{name}`")
    } else {
        anyhow!(
            "unknown subcommand `{name}` for `{parent}`; available subcommands: {}",
            available.join(", ")
        )
    }
}

fn describe_command(command: &Command, path: &[String]) -> Value {
    let segments = path.iter().map(String::as_str).collect::<Vec<_>>();
    let (api_token, pod) = requirements(&segments);
    let (effect, effect_description) = effect(&segments);
    let arguments = command
        .get_arguments()
        .filter(|argument| visible_argument(argument) && !argument.is_global_set())
        .map(|argument| describe_argument(command, argument))
        .collect::<Vec<_>>();
    let subcommands = visible_subcommands(command)
        .map(|subcommand| {
            let mut subcommand_path = path.to_vec();
            subcommand_path.push(subcommand.get_name().to_owned());
            describe_command(subcommand, &subcommand_path)
        })
        .collect::<Vec<_>>();

    json!({
        "name": command.get_name(),
        "path": path,
        "invocation": if path.is_empty() {
            "brainpod".to_owned()
        } else {
            format!("brainpod {}", path.join(" "))
        },
        "summary": command
            .get_long_about()
            .or_else(|| command.get_about())
            .map(ToString::to_string),
        "usage": usage(command),
        "requirements": {
            "apiToken": api_token,
            "pod": pod
        },
        "effect": effect,
        "effectDescription": effect_description,
        "arguments": arguments,
        "subcommands": subcommands,
        "examples": examples(&segments),
        "nextSteps": next_steps(&segments)
    })
}

fn visible_argument(argument: &Arg) -> bool {
    !argument.is_hide_set() && !matches!(argument.get_id().as_str(), "help" | "version")
}

fn visible_subcommands(command: &Command) -> impl Iterator<Item = &Command> {
    command
        .get_subcommands()
        .filter(|subcommand| !subcommand.is_hide_set() && subcommand.get_name() != "help")
}

fn usage(command: &Command) -> String {
    let mut rendered_command = command.clone();
    let rendered = rendered_command.render_usage().to_string();
    rendered
        .trim()
        .strip_prefix("Usage: ")
        .unwrap_or(command.get_name())
        .to_owned()
}

fn describe_argument(command: &Command, argument: &Arg) -> Value {
    let takes_value = argument.get_action().takes_values();
    let value_names = argument
        .get_value_names()
        .map(|names| names.iter().map(ToString::to_string).collect::<Vec<_>>())
        .unwrap_or_else(|| {
            if takes_value { vec![argument.get_id().as_str().to_ascii_uppercase()] } else { Default::default() }
        });
    let defaults = argument
        .get_default_values()
        .iter()
        .map(|value| value.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let possible_values = argument
        .get_possible_values()
        .into_iter()
        .filter(|value| !value.is_hide_set())
        .map(|value| value.get_name().to_owned())
        .collect::<Vec<_>>();
    let conflicts = command
        .get_arg_conflicts_with(argument)
        .into_iter()
        .map(|argument| argument.get_id().as_str().to_owned())
        .collect::<Vec<_>>();
    let num_values = argument.get_num_args().map(|range| {
        json!({
            "min": range.min_values(),
            "max": if range.max_values() == usize::MAX {
                Value::Null
            } else {
                json!(range.max_values())
            }
        })
    });

    let mut value = Map::new();
    value.insert("id".to_owned(), json!(argument.get_id().as_str()));
    value.insert("syntax".to_owned(), json!(argument_syntax(argument, &value_names)));
    value.insert(
        "kind".to_owned(),
        json!(if argument.get_index().is_some() {
            "positional"
        } else {
            "option"
        }),
    );
    value.insert("required".to_owned(), json!(argument.is_required_set()));
    value.insert("global".to_owned(), json!(argument.is_global_set()));
    value.insert("takesValue".to_owned(), json!(takes_value));
    value.insert("valueNames".to_owned(), json!(value_names));
    value.insert("numValues".to_owned(), json!(num_values));
    value.insert(
        "help".to_owned(),
        json!(argument
            .get_long_help()
            .or_else(|| argument.get_help())
            .map(ToString::to_string)),
    );
    value.insert("defaultValues".to_owned(), json!(defaults));
    value.insert("possibleValues".to_owned(), json!(possible_values));
    value.insert("conflictsWith".to_owned(), json!(conflicts));
    Value::Object(value)
}

fn argument_syntax(argument: &Arg, value_names: &[String]) -> String {
    let mut names = Vec::new();
    if let Some(short) = argument.get_short() {
        names.push(format!("-{short}"));
    }
    if let Some(long) = argument.get_long() {
        names.push(format!("--{long}"));
    }

    let value = value_names
        .iter()
        .map(|name| format!("<{name}>"))
        .collect::<Vec<_>>()
        .join(" ");
    let repeated = argument
        .get_num_args()
        .is_some_and(|range| range.max_values() > 1);
    let value = if repeated && !value.is_empty() {
        format!("{value}...")
    } else {
        value
    };

    if names.is_empty() {
        value
    } else if value.is_empty() {
        names.join(", ")
    } else {
        format!("{} {value}", names.join(", "))
    }
}

fn requirements(path: &[&str]) -> (Option<bool>, Option<bool>) {
    match path {
        [] => (None, None),
        ["describe"] | ["login"] | ["config"] | ["config", _] => (Some(false), Some(false)),
        ["blueprint"] => (Some(true), None),
        ["blueprint", "install"] => (Some(true), Some(true)),
        ["blueprint", _]
        | ["whoami"]
        | ["cluster"]
        | ["cluster", _]
        | ["pod"]
        | ["pod", _] => (Some(true), Some(false)),
        ["image"]
        | ["image", _]
        | ["revision"]
        | ["revision", _]
        | ["resource"]
        | ["resource", _]
        | ["deploy"]
        | ["redeploy"]
        | ["events"] => (Some(true), Some(true)),
        _ => (None, None),
    }
}

fn effect(path: &[&str]) -> (&'static str, &'static str) {
    match path {
        ["describe"] | ["config", "show"] | ["config", "path"] => {
            ("read", "Reads local CLI information without changing state.")
        }
        ["config", "set"] | ["config", "unset"] => {
            ("local-write", "Changes the local CLI configuration.")
        }
        ["login"] => (
            "local-and-remote-write",
            "Authorizes in the dashboard and stores the API token locally.",
        ),
        ["whoami"]
        | ["cluster", "list"]
        | ["pod", "list"]
        | ["pod", "get"]
        | ["blueprint", "list"]
        | ["blueprint", "get"]
        | ["revision", _]
        | ["resource", "list"]
        | ["resource", "get"] => ("read", "Reads remote state without changing it."),
        ["events"] => ("read-or-stream", "Reads or continuously streams remote events."),
        ["image", "list"] | ["image", "inspect"] => (
            "read",
            "Reads active registry images visible from the selected pod.",
        ),
        ["image", "build"] => (
            "local-and-remote-write",
            "Builds locally and pushes an image to the selected pod's private registry namespace.",
        ),
        ["pod", "create"] => ("remote-write", "Creates a remote pod."),
        ["blueprint", "install"] | ["resource", "replace"] | ["resource", "delete"] => (
            "draft-write",
            "Changes the pod's mutable draft without deploying it.",
        ),
        ["resource", "create"] => (
            "conditional-draft-write",
            "Changes the pod's mutable draft unless --dry-run is supplied; it does not deploy.",
        ),
        ["deploy"] => ("deployment", "Deploys the pod's current mutable draft."),
        ["redeploy"] => ("deployment", "Redeploys the currently deployed revision."),
        _ => ("mixed", "Effect depends on the selected subcommand."),
    }
}

fn next_steps(path: &[&str]) -> Vec<&'static str> {
    match path {
        ["login"] => vec!["Confirm the authenticated identity with `brainpod whoami`."],
        ["pod", "create"] => vec![
            "Select the new pod with --pod or `brainpod config set pod <pod>`.",
        ],
        ["image", "list"] => vec![
            "Inspect an exact pod image with `brainpod image inspect <repository> <tag>`; use --visibility public for a public image.",
        ],
        ["image", "inspect"] => vec![
            "Use a returned digest-pinned variant reference as an App resource's spec.image.",
        ],
        ["image", "build"] => vec![
            "Use the returned digest-pinned reference as an App resource's spec.image.",
            "Deploy the updated App resource with `brainpod deploy` when ready.",
        ],
        ["blueprint", "install"]
        | ["resource", "create"]
        | ["resource", "replace"]
        | ["resource", "delete"] => vec![
            "Inspect the updated draft with `brainpod resource list`.",
            "Deploy it with `brainpod deploy` when ready.",
        ],
        ["resource", "list"] | ["resource", "get"] => vec![
            "Use a returned resource URN with `brainpod events --resource <urn>`.",
        ],
        _ => Vec::new(),
    }
}

fn examples(path: &[&str]) -> Vec<&'static str> {
    match path {
        ["describe"] => vec![
            "brainpod describe",
            "brainpod describe resource create",
            "brainpod describe resource create --json",
        ],
        ["login"] => vec!["brainpod login"],
        ["config", "set"] => vec![
            "brainpod config set api-key brain_example",
            "brainpod config set pod my-pod",
        ],
        ["pod", "list"] => vec!["brainpod pod list --json"],
        ["blueprint", "get"] => vec!["brainpod blueprint get laravel"],
        ["blueprint", "install"] => vec![
            "brainpod --pod my-pod blueprint install laravel",
            "brainpod --pod my-pod blueprint install laravel --file blueprint-input.json",
        ],
        ["image", "list"] => vec![
            "brainpod --pod my-pod image list",
            "brainpod --pod my-pod image list --visibility pod --limit 10 --json",
        ],
        ["image", "inspect"] => vec![
            "brainpod --pod my-pod image inspect api v1",
            "brainpod --pod my-pod image inspect ubuntu latest --visibility public --json",
        ],
        ["image", "build"] => vec![
            "brainpod --pod my-pod image build api .",
            "brainpod --pod my-pod image build api ./services/api --builder railpack --tag v1 --output ./api.oci --json",
        ],
        ["resource", "create"] => vec![
            "brainpod --pod my-pod resource create --file resources.json --dry-run --json",
            "brainpod --pod my-pod resource create --file resources.json --json",
        ],
        ["revision", "wait"] => vec![
            "brainpod --pod my-pod revision wait <revision>",
            "brainpod --pod my-pod revision wait <revision> --timeout 180 --json",
        ],
        ["deploy"] => vec![
            "brainpod --pod my-pod deploy --summary \"Configure application resources\" --wait --json",
        ],
        ["events"] => vec![
            "brainpod --pod my-pod events --resource urn:brain:app:default:api",
            "brainpod --pod my-pod events --watch --resource urn:brain:app:default:api --json",
        ],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory as _;

    use super::generate;

    #[test]
    fn describes_a_leaf_command() {
        let path = vec!["resource".to_owned(), "create".to_owned()];
        let value = generate(crate::Opts::command(), &path).unwrap();

        assert_eq!(
            value.pointer("/command/invocation").and_then(|value| value.as_str()),
            Some("brainpod resource create")
        );
        assert_eq!(
            value.pointer("/command/requirements/pod").and_then(|value| value.as_bool()),
            Some(true)
        );
        assert_eq!(
            value.pointer("/command/effect").and_then(|value| value.as_str()),
            Some("conditional-draft-write")
        );
    }

    #[test]
    fn describes_image_build_as_authenticated_remote_write() {
        let path = vec!["image".to_owned(), "build".to_owned()];
        let value = generate(crate::Opts::command(), &path).unwrap();

        assert_eq!(
            value.pointer("/command/requirements/apiToken").and_then(|value| value.as_bool()),
            Some(true)
        );
        assert_eq!(
            value.pointer("/command/requirements/pod").and_then(|value| value.as_bool()),
            Some(true)
        );
        assert_eq!(
            value.pointer("/command/effect").and_then(|value| value.as_str()),
            Some("local-and-remote-write")
        );
    }

    #[test]
    fn describes_image_list_as_authenticated_read() {
        let path = vec!["image".to_owned(), "list".to_owned()];
        let value = generate(crate::Opts::command(), &path).unwrap();

        assert_eq!(
            value.pointer("/command/requirements/apiToken").and_then(|value| value.as_bool()),
            Some(true)
        );
        assert_eq!(
            value.pointer("/command/requirements/pod").and_then(|value| value.as_bool()),
            Some(true)
        );
        assert_eq!(
            value.pointer("/command/effect").and_then(|value| value.as_str()),
            Some("read")
        );
    }

    #[test]
    fn rejects_an_unknown_command_path() {
        let path = vec!["missing".to_owned()];
        let error = generate(crate::Opts::command(), &path).unwrap_err();

        assert!(error.to_string().contains("unknown subcommand `missing`"));
    }
}
