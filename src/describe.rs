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
            "tunnelJson": "Tunnels emit listening, optional credentials, and closed events as NDJSON.",
            "waitProgress": "Interactive waits report unhealthy-to-healthy transitions on stderr; progress is suppressed when stderr is redirected or --json is used.",
            "s3Download": "s3 download <source> - writes raw bytes only, even with --json; errors remain on stderr.",
            "errors": "Errors are written to stderr and return a non-zero exit code; --json also makes errors JSON."
        },
        "guidance": [
            "Use --json for complete machine-readable API responses and errors; login, event watches, and tunnels are streamed as NDJSON.",
            "Pod-scoped commands require --pod, BRAINPOD_POD, or a configured default pod.",
            "Database tunnels resolve a resource name, URN, or stable UUID in the selected pod and listen on loopback by default until Ctrl-C is pressed.",
            "Image builds prefer an existing Dockerfile, otherwise use Railpack, target the best architecture supported by the API (override with --platform), and push to the selected pod's private registry namespace.",
            "Blueprint installation and resource mutations update the mutable draft; run deploy separately when ready, optionally with --wait.",
            "Use blueprint get to inspect blueprint documentation, defaults, and its input schema before installation.",
            "Event queries accept a resource name, URN, or stable UUID in the selected pod.",
            "Use `brainpod describe resource <kind>` to inspect the resource schema and the variables that kind exports, fetched from the production OpenAPI document; the embedded document is used when it is unavailable. It needs no pod and no API token, so run it before creating anything.",
            "Use `brainpod --pod <pod> resource variables` to read the resolved references of resources that already exist; secret values are never returned.",
            "Use `brainpod api <path>` only for API endpoints without a first-class command; the path is relative to the configured endpoint, so the API token is never sent elsewhere."
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
            if takes_value {
                vec![argument.get_id().as_str().to_ascii_uppercase()]
            } else {
                Default::default()
            }
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
    value.insert(
        "syntax".to_owned(),
        json!(argument_syntax(argument, &value_names)),
    );
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
        json!(
            argument
                .get_long_help()
                .or_else(|| argument.get_help())
                .map(ToString::to_string)
        ),
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
        ["describe"] | ["detect"] | ["login"] | ["config"] | ["config", _] => {
            (Some(false), Some(false))
        }
        ["blueprint"] | ["api"] => (Some(true), None),
        ["blueprint", "install"] => (Some(true), Some(true)),
        ["blueprint", _]
        | ["whoami"]
        | ["cluster"]
        | ["cluster", _]
        | ["storage"]
        | ["storage", _]
        | ["github"]
        | ["github", _]
        | ["pod"]
        | ["pod", _] => (Some(true), Some(false)),
        ["s3"] | ["s3", _] => (Some(true), Some(true)),
        ["image"]
        | ["image", _]
        | ["revision"]
        | ["revision", _]
        | ["resource"]
        | ["resource", _]
        | ["tunnel"]
        | ["deploy"]
        | ["redeploy"]
        | ["rollback"]
        | ["events"]
        | ["repo"]
        | ["repo", _]
        | ["onboard"]
        | ["runs"]
        | ["runs", _]
        | ["sessions"]
        | ["sessions", _] => (Some(true), Some(true)),
        _ => (None, None),
    }
}

fn effect(path: &[&str]) -> (&'static str, &'static str) {
    match path {
        ["detect"] => (
            "read",
            "Reads local project manifests without executing project code or accessing the network.",
        ),
        ["describe"] | ["config", "show"] | ["config", "path"] => (
            "read",
            "Reads local CLI information without changing state.",
        ),
        ["config", "set"] | ["config", "unset"] => {
            ("local-write", "Changes the local CLI configuration.")
        }
        ["login"] => (
            "local-and-remote-write",
            "Authorizes in the dashboard and stores the API token locally.",
        ),
        ["github", "auth"] | ["github", "repos"] => (
            "read",
            "Reads which GitHub accounts and repositories the Brainpod GitHub App can access; it does not authenticate or change GitHub access.",
        ),
        ["repo", "get"] | ["runs", "list"] | ["runs", "get"] | ["sessions", _] => {
            ("read", "Reads remote state without changing it.")
        }
        ["repo", "link"] => (
            "remote-write",
            "Links a GitHub repository the Brainpod GitHub App can access to the pod; a pod linked to another repository is refused, never relinked.",
        ),
        ["onboard"] => (
            "conditional-deployment",
            "Starts the pod's repository onboarding: its runs write the pod's draft, commit to the repository's brainpod/onboarding-<pod> branch, open a draft pull request, and deploy the pod unless a run needs input. Refused on a pod that was ever deployed.",
        ),
        ["runs", "cancel"] => (
            "remote-write",
            "Cancels an agent run that has not finished.",
        ),
        ["runs", "retry"] | ["runs", "reply"] | ["runs", "answer"] => (
            "conditional-deployment",
            "Queues or resumes an agent run; a repository onboarding continues and deploys the pod unless a run needs input.",
        ),
        ["whoami"]
        | ["cluster", "list"]
        | ["storage", "providers"]
        | ["pod", "list"]
        | ["pod", "get"]
        | ["blueprint", "list"]
        | ["blueprint", "get"]
        | ["revision", _]
        | ["resource", "list"]
        | ["resource", "get"]
        | ["resource", "variables"] => ("read", "Reads remote state without changing it."),
        ["s3", "ls"] => (
            "read",
            "Lists gateway buckets or one level of object prefixes.",
        ),
        ["s3", "download"] => (
            "read-stream",
            "Streams provider object bytes to stdout without a text or JSON summary.",
        ),
        ["s3", _] => (
            "local-and-remote-write",
            "Transfers one bounded object or deletes one exact key; moves delete only after a successful copy.",
        ),
        ["events"] => (
            "read-or-stream",
            "Reads or continuously streams remote events.",
        ),
        ["tunnel"] => (
            "local-and-remote-stream",
            "Creates a remote tunnel session and forwards local TCP connections until Ctrl-C.",
        ),
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
        ["redeploy"] => (
            "deployment",
            "Retries deploying the pod's current head after its deployment failed.",
        ),
        ["rollback"] => (
            "conditional-deployment",
            "Replaces the pod's draft head with a copy of an earlier revision, replacing any undeployed draft changes; deploys it only with --deploy.",
        ),
        ["api"] => (
            "conditional-remote-write",
            "GET reads remote state; other methods can change remote state, including deploying.",
        ),
        _ => ("mixed", "Effect depends on the selected subcommand."),
    }
}

fn next_steps(path: &[&str]) -> Vec<&'static str> {
    match path {
        ["login"] => vec!["Confirm the authenticated identity with `brainpod whoami`."],
        ["github", "auth"] => vec![
            "When GitHub is not connected, give the user the `_links.connect` URL; connecting, granting repositories, and accepting permissions happen in the browser, never through the CLI.",
            "Find a repository to link to a Pod with `brainpod github repos [QUERY]`.",
        ],
        ["github", "repos"] => vec![
            "A listed repository can be linked to a Pod; with `--json`, its `id` and `installationId` identify the repository to link.",
            "When a repository is missing, give the user the settings URL of a `selected` installation to grant it, or the `_links.connect` URL to add another account or organization.",
        ],
        ["pod", "create"] => vec![
            "Select the new pod with --pod or `brainpod config set pod <pod>`.",
            "Onboard a GitHub repository onto it with `brainpod --pod <pod> repo link OWNER/NAME`, then `brainpod --pod <pod> onboard`.",
        ],
        ["repo", "link"] => vec![
            "Start onboarding the linked repository with `brainpod --pod <pod> onboard [--ref REF]`.",
            "When the repository is not accessible, give the user the printed settings URL of a `selected` installation to grant it, or the connect URL to add another account or organization; `brainpod github repos` lists what is accessible.",
        ],
        ["repo", "get"] => vec![
            "Without a linked repository, link one with `brainpod --pod <pod> repo link OWNER/NAME`.",
        ],
        ["onboard"] => vec![
            "Follow the onboarding with `brainpod --pod <pod> sessions get <sessionId>`, using the returned run's `sessionId`.",
            "Answer a run awaiting input with `brainpod --pod <pod> runs answer <run> --answer <questionId>=<answer>`; retry a failed run with `brainpod --pod <pod> runs retry <run>`.",
            "Once the Pod is healthy, the user merges the continuous-deploys pull request so every push to the onboarded branch deploys.",
        ],
        ["runs", "list"] | ["runs", "get"] => vec![
            "Answer a run in state awaiting_input with `brainpod --pod <pod> runs answer <run> --answer <questionId>=<answer>` for each of its `questions`.",
            "Follow a run's session with `brainpod --pod <pod> sessions get <sessionId>`.",
        ],
        ["runs", "cancel"] => {
            vec!["Retry a canceled onboarding run with `brainpod --pod <pod> runs retry <run>`."]
        }
        ["runs", "retry"] | ["runs", "reply"] | ["runs", "answer"] => vec![
            "Follow the run with `brainpod --pod <pod> sessions get <sessionId>` or `brainpod --pod <pod> runs get <run>`.",
        ],
        ["sessions", "list"] => vec![
            "Find the pod's repository onboarding with --label purpose=repo-onboarding, then read it with `brainpod --pod <pod> sessions get <session>`.",
        ],
        ["sessions", "get"] => vec![
            "Answer open questions with `brainpod --pod <pod> runs answer <run> --answer <questionId>=<answer>`; retry the latest failed run with `brainpod --pod <pod> runs retry <run>`; request changes to a reviewed draft with `brainpod --pod <pod> runs reply <run> <message>`.",
            "When `pullRequest` is open, the user merges it once the Pod is healthy so every push to its `base` deploys; closing it turns deploy on push off.",
        ],
        ["image", "list"] => vec![
            "Inspect an exact pod image with `brainpod image inspect <repository> <tag>`; use --visibility public for a public image.",
        ],
        ["image", "inspect"] => {
            vec!["Use a returned digest-pinned variant reference as an App resource's spec.image."]
        }
        ["image", "build"] => vec![
            "Use the returned digest-pinned reference as an App resource's spec.image.",
            "Deploy the updated App resource with `brainpod deploy` when ready.",
        ],
        ["rollback"] => vec![
            "Find the revision to restore with `brainpod revision list`.",
            "Without --deploy, compare the restored draft with what runs using `brainpod revision diff <draft> --base <deployed>`, then deploy it with `brainpod deploy` when ready.",
        ],
        ["blueprint", "install"]
        | ["resource", "create"]
        | ["resource", "replace"]
        | ["resource", "delete"] => vec![
            "Inspect the updated draft with `brainpod resource list`.",
            "Read the references other resources can use with `brainpod resource variables`.",
            "Discover the variables a kind exports before creating it with `brainpod describe resource <kind>`; it needs no pod and no API token.",
            "Choose a Bucket's spec.provider and spec.region from `brainpod storage providers`.",
            "Deploy it with `brainpod deploy` when ready.",
        ],
        ["resource", "list"] | ["resource", "get"] => vec![
            "Use the resource name or returned URN with `brainpod events --resource <resource>`.",
            "Read the resolved references with `brainpod resource variables`.",
        ],
        ["resource", "variables"] => vec![
            "Reference a variable from an App resource's spec.env as its `ref` value, such as ${db.uri}.",
            "Secret values are never returned; reference them instead of reading them.",
        ],
        ["tunnel"] => vec![
            "Keep the tunnel running while the local database client is connected.",
            "Press Ctrl-C to close the tunnel session.",
        ],
        ["api"] => vec![
            "Prefer a first-class command when one exists; use `brainpod api` only for endpoints the CLI does not cover.",
            "The contract for every endpoint is the API's OpenAPI document at <endpoint>/v1/openapi.json, the document `brainpod describe resource <kind>` reads; fetch it with `brainpod api /v1/openapi.json`.",
            "A pod is only required when PATH contains {pod}.",
        ],
        _ => Vec::new(),
    }
}

fn examples(path: &[&str]) -> Vec<&'static str> {
    match path {
        ["detect"] => vec!["brainpod detect .", "brainpod detect ./services --json"],
        ["github", "auth"] => vec!["brainpod github auth", "brainpod github auth --json"],
        ["github", "repos"] => vec![
            "brainpod github repos",
            "brainpod github repos acme/ --page 2 --per-page 50 --json",
        ],
        ["repo", "link"] => vec![
            "brainpod --pod my-pod repo link acme/web",
            "brainpod --pod my-pod repo link acme/web --json",
        ],
        ["repo", "get"] => vec!["brainpod --pod my-pod repo get --json"],
        ["onboard"] => vec![
            "brainpod --pod my-pod onboard",
            "brainpod --pod my-pod onboard --ref release --json",
        ],
        ["runs", "list"] => vec!["brainpod --pod my-pod runs list --json"],
        ["runs", "get"] => vec!["brainpod --pod my-pod runs get <run> --json"],
        ["runs", "cancel"] => vec!["brainpod --pod my-pod runs cancel <run>"],
        ["runs", "retry"] => vec!["brainpod --pod my-pod runs retry <run>"],
        ["runs", "reply"] => {
            vec!["brainpod --pod my-pod runs reply <run> \"Use Valkey instead of Redis\""]
        }
        ["runs", "answer"] => vec![
            "brainpod --pod my-pod runs answer <run> --answer database=postgres --answer domain=shop.example.com",
        ],
        ["sessions", "list"] => vec![
            "brainpod --pod my-pod sessions list --label purpose=repo-onboarding --status active",
        ],
        ["sessions", "get"] => vec![
            "brainpod --pod my-pod sessions get <session>",
            "brainpod --pod my-pod sessions get <session> --json",
        ],
        ["describe"] => vec![
            "brainpod describe",
            "brainpod describe resource create",
            "brainpod describe resource create --json",
            "brainpod describe resource postgres",
            "brainpod describe resource --json",
        ],
        ["login"] => vec!["brainpod login"],
        ["config", "set"] => vec![
            "brainpod config set api-key brain_example",
            "brainpod config set pod my-pod",
        ],
        ["pod", "list"] => vec!["brainpod pod list --json"],
        ["tunnel"] => vec![
            "brainpod --pod my-pod tunnel db",
            "brainpod --pod my-pod tunnel urn:brain:postgres:default:db 127.0.0.1:15432",
        ],
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
        ["resource", "variables"] => vec![
            "brainpod --pod my-pod resource variables",
            "brainpod --pod my-pod resource variables postgres db --json",
        ],
        ["revision", "wait"] => vec![
            "brainpod --pod my-pod revision wait <revision>",
            "brainpod --pod my-pod revision wait <revision> --timeout 180 --json",
        ],
        ["deploy"] => vec![
            "brainpod --pod my-pod deploy --summary \"Configure application resources\" --wait --json",
        ],
        ["rollback"] => vec![
            "brainpod --pod my-pod rollback <revision>",
            "brainpod --pod my-pod rollback <revision> --deploy --wait --json",
        ],
        ["events"] => vec![
            "brainpod --pod my-pod events --resource api",
            "brainpod --pod my-pod events --watch --resource urn:brain:app:default:api --json",
        ],
        ["api"] => vec![
            "brainpod --pod my-pod api /v1/pods/{pod}/builds",
            "brainpod api /v1/onboarding",
            "brainpod --pod my-pod api -X POST /v1/pods/{pod}/secrets --input secret.json",
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
            value
                .pointer("/command/invocation")
                .and_then(|value| value.as_str()),
            Some("brainpod resource create")
        );
        assert_eq!(
            value
                .pointer("/command/requirements/pod")
                .and_then(|value| value.as_bool()),
            Some(true)
        );
        assert_eq!(
            value
                .pointer("/command/effect")
                .and_then(|value| value.as_str()),
            Some("conditional-draft-write")
        );
    }

    #[test]
    fn describes_image_build_as_authenticated_remote_write() {
        let path = vec!["image".to_owned(), "build".to_owned()];
        let value = generate(crate::Opts::command(), &path).unwrap();

        assert_eq!(
            value
                .pointer("/command/requirements/apiToken")
                .and_then(|value| value.as_bool()),
            Some(true)
        );
        assert_eq!(
            value
                .pointer("/command/requirements/pod")
                .and_then(|value| value.as_bool()),
            Some(true)
        );
        assert_eq!(
            value
                .pointer("/command/effect")
                .and_then(|value| value.as_str()),
            Some("local-and-remote-write")
        );
    }

    #[test]
    fn describes_image_list_as_authenticated_read() {
        let path = vec!["image".to_owned(), "list".to_owned()];
        let value = generate(crate::Opts::command(), &path).unwrap();

        assert_eq!(
            value
                .pointer("/command/requirements/apiToken")
                .and_then(|value| value.as_bool()),
            Some(true)
        );
        assert_eq!(
            value
                .pointer("/command/requirements/pod")
                .and_then(|value| value.as_bool()),
            Some(true)
        );
        assert_eq!(
            value
                .pointer("/command/effect")
                .and_then(|value| value.as_str()),
            Some("read")
        );
    }

    #[test]
    fn describes_resource_variables_as_an_authenticated_read() {
        let path = vec!["resource".to_owned(), "variables".to_owned()];
        let value = generate(crate::Opts::command(), &path).unwrap();

        assert_eq!(
            value
                .pointer("/command/effect")
                .and_then(|value| value.as_str()),
            Some("read")
        );
        assert_eq!(
            value
                .pointer("/command/requirements/apiToken")
                .and_then(|value| value.as_bool()),
            Some(true)
        );
        assert_eq!(
            value
                .pointer("/command/requirements/pod")
                .and_then(|value| value.as_bool()),
            Some(true)
        );
        assert!(
            !value
                .pointer("/command/examples")
                .and_then(|value| value.as_array())
                .is_none_or(Vec::is_empty)
        );
        assert!(
            !value
                .pointer("/command/nextSteps")
                .and_then(|value| value.as_array())
                .is_none_or(Vec::is_empty)
        );
    }

    #[test]
    fn points_resource_mutations_at_the_variable_catalog() {
        let path = vec!["resource".to_owned(), "create".to_owned()];
        let value = generate(crate::Opts::command(), &path).unwrap();
        let next_steps = value
            .pointer("/command/nextSteps")
            .and_then(|value| value.as_array())
            .unwrap()
            .iter()
            .filter_map(|step| step.as_str())
            .collect::<Vec<_>>();

        assert!(
            next_steps
                .iter()
                .any(|step| step.contains("brainpod resource variables"))
        );
        assert!(
            next_steps
                .iter()
                .any(|step| step.contains("brainpod describe resource <kind>"))
        );
    }

    #[test]
    fn describes_api_as_an_authenticated_conditional_write_with_an_optional_pod() {
        let path = vec!["api".to_owned()];
        let value = generate(crate::Opts::command(), &path).unwrap();

        assert_eq!(
            value
                .pointer("/command/requirements/apiToken")
                .and_then(|value| value.as_bool()),
            Some(true)
        );
        assert!(
            value
                .pointer("/command/requirements/pod")
                .is_some_and(|value| value.is_null())
        );
        assert_eq!(
            value
                .pointer("/command/effect")
                .and_then(|value| value.as_str()),
            Some("conditional-remote-write")
        );
    }

    #[test]
    fn describes_github_as_authenticated_commands_without_a_pod() {
        for (path, effect) in [(["github", "auth"], "read"), (["github", "repos"], "read")] {
            let path = path.map(str::to_owned).to_vec();
            let value = generate(crate::Opts::command(), &path).unwrap();

            assert_eq!(
                value
                    .pointer("/command/requirements/apiToken")
                    .and_then(|value| value.as_bool()),
                Some(true),
                "{path:?}"
            );
            assert_eq!(
                value
                    .pointer("/command/requirements/pod")
                    .and_then(|value| value.as_bool()),
                Some(false),
                "{path:?}"
            );
            assert_eq!(
                value
                    .pointer("/command/effect")
                    .and_then(|value| value.as_str()),
                Some(effect),
                "{path:?}"
            );
            for key in ["examples", "nextSteps"] {
                assert!(
                    !value
                        .pointer(&format!("/command/{key}"))
                        .and_then(|value| value.as_array())
                        .is_none_or(Vec::is_empty),
                    "{path:?} {key}"
                );
            }
        }
    }

    #[test]
    fn describes_the_onboarding_commands_as_pod_scoped() {
        for (path, effect) in [
            (&["repo", "link"][..], "remote-write"),
            (&["repo", "get"], "read"),
            (&["onboard"], "conditional-deployment"),
            (&["runs", "list"], "read"),
            (&["runs", "get"], "read"),
            (&["runs", "cancel"], "remote-write"),
            (&["runs", "retry"], "conditional-deployment"),
            (&["runs", "reply"], "conditional-deployment"),
            (&["runs", "answer"], "conditional-deployment"),
            (&["sessions", "list"], "read"),
            (&["sessions", "get"], "read"),
        ] {
            let path = path
                .iter()
                .map(|segment| (*segment).to_owned())
                .collect::<Vec<_>>();
            let value = generate(crate::Opts::command(), &path).unwrap();

            for requirement in ["apiToken", "pod"] {
                assert_eq!(
                    value
                        .pointer(&format!("/command/requirements/{requirement}"))
                        .and_then(|value| value.as_bool()),
                    Some(true),
                    "{path:?} {requirement}"
                );
            }
            assert_eq!(
                value
                    .pointer("/command/effect")
                    .and_then(|value| value.as_str()),
                Some(effect),
                "{path:?}"
            );
            for key in ["examples", "nextSteps"] {
                assert!(
                    !value
                        .pointer(&format!("/command/{key}"))
                        .and_then(|value| value.as_array())
                        .is_none_or(Vec::is_empty),
                    "{path:?} {key}"
                );
            }
        }
    }

    #[test]
    fn rejects_an_unknown_command_path() {
        let path = vec!["missing".to_owned()];
        let error = generate(crate::Opts::command(), &path).unwrap_err();

        assert!(error.to_string().contains("unknown subcommand `missing`"));
    }
}
