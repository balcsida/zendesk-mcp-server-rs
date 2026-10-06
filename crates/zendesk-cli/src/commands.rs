//! `zendesk <group> <operation>` commands, built at run time from the API catalog.

use std::collections::HashSet;

use anyhow::{Result, bail};
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{Map, Value};
use zendesk::ZendeskClient;
use zendesk::catalog::{self, Location, Operation, Param};

use crate::{http_client, parse_query, print_json, read_data, resolve_auth};

/// Option names that catalog parameters may not take: they are reached through `--param`.
const RESERVED_OPTIONS: [&str; 4] = ["data", "param", "help", "version"];

/// clap wants `&'static str` names without its `string` feature. The tree lives as long
/// as the process, so leaking is free.
fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

/// Keep alphanumeric words, joined by single dashes.
fn join_words(s: &str) -> String {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// `ListSLAPolicies` → `list-sla-policies`, `Ticket Comments` → `ticket-comments`.
/// Acronyms stay whole, and a plural `s` stays with its acronym (`ListIVRs` → `list-ivrs`).
fn kebab(s: &str) -> String {
    let chars: Vec<char> = s.replace("OAuth", "Oauth").chars().collect();
    let mut out = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c.is_uppercase() && i > 0 {
            let prev = chars[i - 1];
            let next = chars.get(i + 1);
            let plural = next == Some(&'s') && !chars.get(i + 2).is_some_and(|n| n.is_lowercase());
            if prev.is_lowercase()
                || prev.is_ascii_digit()
                || (prev.is_uppercase() && next.is_some_and(|n| n.is_lowercase()) && !plural)
            {
                out.push('-');
            }
        }
        out.extend(c.to_lowercase());
    }
    join_words(&out)
}

/// `page[size]` → `page-size`, `filter[source_type]` → `filter-source-type`.
fn option_name(param: &str) -> String {
    join_words(&param.to_lowercase())
}

/// `ticket_id` → `TICKET_ID`.
fn value_name(param: &str) -> String {
    param
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// The path parameters, in path order.
fn path_names(op: &Operation) -> Vec<&str> {
    let mut names: Vec<&str> = Vec::new();
    for part in op.path.split('{').skip(1) {
        if let Some((name, _)) = part.split_once('}')
            && !names.contains(&name)
        {
            names.push(name);
        }
    }
    names
}

/// The query parameters that get their own option, with its name. Those whose name is
/// reserved or already taken by an earlier parameter are left to `--param`.
fn query_options(op: &Operation) -> Vec<(&Param, String)> {
    let mut taken: HashSet<String> = RESERVED_OPTIONS.iter().map(|s| s.to_string()).collect();
    op.params
        .iter()
        .filter(|p| p.location == Location::Query)
        .filter_map(|p| {
            let name = option_name(&p.name);
            (!name.is_empty() && taken.insert(name.clone())).then_some((p, name))
        })
        .collect()
}

fn operation_command(op: &Operation) -> Command {
    let mut long_about = format!("{}\n\n{} {}", op.summary, op.method, op.path);
    if !op.description.is_empty() {
        long_about.push_str(&format!("\n\n{}", op.description));
    }
    if let Some(example) = op.body.as_ref().and_then(|b| b.example.as_ref()) {
        long_about.push_str(&format!(
            "\n\nExample body:\n{}",
            serde_json::to_string_pretty(example).unwrap_or_default()
        ));
    }
    let mut cmd = Command::new(leak(kebab(&op.id)))
        .about(op.summary.clone())
        .long_about(long_about);

    for name in path_names(op) {
        let help = op
            .params
            .iter()
            .find(|p| p.location == Location::Path && p.name == name)
            .map(|p| p.description.clone())
            .unwrap_or_default();
        cmd = cmd.arg(
            Arg::new(leak(format!("path:{name}")))
                .value_name(leak(value_name(name)))
                .help(help)
                .required(true),
        );
    }

    for (param, name) in query_options(op) {
        let mut help = param.description.clone();
        if !param.values.is_empty() {
            help = format!("{help} One of: {}.", param.values.join(", "))
                .trim()
                .to_string();
        }
        cmd = cmd.arg(
            Arg::new(leak(format!("query:{name}")))
                .long(leak(name.clone()))
                .value_name(leak(value_name(&name)))
                .help(help)
                .action(ArgAction::Append)
                .required(param.required),
        );
    }

    cmd = cmd.arg(
        Arg::new("param")
            .short('p')
            .long("param")
            .value_name("KEY=VALUE")
            .help("Query parameter the catalog does not list; repeatable")
            .action(ArgAction::Append)
            .value_parser(parse_query),
    );
    if !op.is_read() {
        cmd = cmd.arg(
            Arg::new("data")
                .short('d')
                .long("data")
                .value_name("JSON")
                .help("JSON request body: inline, `@file`, or `@-` for stdin")
                .required(op.body.as_ref().is_some_and(|b| b.required)),
        );
    }
    cmd
}

/// One command per group, each holding that group's operations.
fn groups() -> Vec<Command> {
    let mut groups: Vec<(&str, Command)> = Vec::new();
    for op in catalog::operations() {
        let at = groups.iter().position(|(g, _)| *g == op.group);
        let at = at.unwrap_or_else(|| {
            let cmd = Command::new(leak(kebab(&op.group)))
                .about(op.group.clone())
                .subcommand_required(true)
                .arg_required_else_help(true);
            groups.push((&op.group, cmd));
            groups.len() - 1
        });
        let cmd = &mut groups[at].1;
        *cmd = std::mem::take(cmd).subcommand(operation_command(op));
    }
    groups.into_iter().map(|(_, cmd)| cmd).collect()
}

/// `base` with a group command for every catalog group.
pub fn command(base: Command) -> Command {
    base.subcommands(groups())
}

/// The operation and its matches when `matches` select a catalog operation.
pub fn selected(matches: &ArgMatches) -> Option<(&'static Operation, &ArgMatches)> {
    let (group, group_matches) = matches.subcommand()?;
    let (name, op_matches) = group_matches.subcommand()?;
    let op = catalog::operations()
        .iter()
        .find(|op| kebab(&op.group) == group && kebab(&op.id) == name)?;
    Some((op, op_matches))
}

/// Add `value` under `key`; a repeated key becomes an array.
fn add(args: &mut Map<String, Value>, key: &str, value: Value) {
    match args.get_mut(key) {
        None => {
            args.insert(key.to_string(), value);
        }
        Some(Value::Array(items)) => items.push(value),
        Some(old) => *old = Value::Array(vec![old.take(), value]),
    }
}

/// The values of one query option: a string, an array for repeats, or for an `object`
/// parameter an object built from `KEY=VALUE` values.
fn query_value(param: &Param, values: Vec<&String>) -> Result<Value> {
    if param.kind == "object" && values.iter().any(|v| v.contains('=')) {
        if let Some(plain) = values.iter().find(|v| !v.contains('=')) {
            bail!(
                "--{} takes KEY=VALUE, got: {plain}",
                option_name(&param.name)
            );
        }
        let mut object = Map::new();
        for value in values {
            let (key, value) = value.split_once('=').unwrap_or_default();
            add(&mut object, key, Value::String(value.to_string()));
        }
        return Ok(Value::Object(object));
    }
    let mut strings: Vec<Value> = values
        .into_iter()
        .map(|v| Value::String(v.clone()))
        .collect();
    Ok(if strings.len() == 1 {
        strings.remove(0)
    } else {
        Value::Array(strings)
    })
}

/// The arguments for `Operation::request` that `matches` hold.
fn collect_args(op: &Operation, matches: &ArgMatches) -> Result<Map<String, Value>> {
    let mut args = Map::new();
    for name in path_names(op) {
        if let Some(value) = matches.get_one::<String>(&format!("path:{name}")) {
            args.insert(name.to_string(), Value::String(value.clone()));
        }
    }
    for (param, name) in query_options(op) {
        if let Some(values) = matches.get_many::<String>(&format!("query:{name}")) {
            args.insert(param.name.clone(), query_value(param, values.collect())?);
        }
    }
    for (key, value) in matches
        .get_many::<(String, String)>("param")
        .into_iter()
        .flatten()
    {
        if path_names(op).contains(&key.as_str()) {
            bail!("{key} is a path parameter: pass it as an argument, not with --param");
        }
        add(&mut args, key, Value::String(value.clone()));
    }
    Ok(args)
}

/// Call the operation `matches` select through `client`.
async fn call(client: &ZendeskClient, op: &Operation, matches: &ArgMatches) -> Result<Value> {
    let args = collect_args(op, matches)?;
    let body = if op.is_read() {
        None
    } else {
        matches
            .get_one::<String>("data")
            .map(|d| read_data(d, &mut std::io::stdin()))
            .transpose()?
    };
    client.call_operation(op, &args, body.as_ref()).await
}

/// Run a catalog operation and print the response like `api` does.
pub async fn run(op: &Operation, matches: &ArgMatches) -> Result<()> {
    let http = http_client()?;
    let (subdomain, auth) = resolve_auth(&http, false).await?;
    let client = ZendeskClient::new(&subdomain, auth, http);
    print_json(&call(&client, op, matches).await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use serde_json::json;
    use wiremock::matchers::{body_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use zendesk::auth::Auth;

    fn tree() -> Command {
        command(crate::Cli::command())
    }

    fn parse(args: &[&str]) -> Result<ArgMatches, clap::Error> {
        tree().try_get_matches_from(std::iter::once("zendesk").chain(args.iter().copied()))
    }

    fn client(server: &MockServer) -> ZendeskClient {
        ZendeskClient::with_base_url(
            "acme",
            Auth::bearer("t"),
            reqwest::Client::new(),
            format!("{}/api/v2", server.uri()),
        )
    }

    #[test]
    fn names_are_kebab_case_with_whole_acronyms() {
        for (id, expected) in [
            ("ShowTicket", "show-ticket"),
            ("TicketsShowMany", "tickets-show-many"),
            ("ListTicketEmailCCs", "list-ticket-email-ccs"),
            ("ShowOAuthClient", "show-oauth-client"),
            ("ListSLAPolicies", "list-sla-policies"),
            ("UpdateIVRMenu", "update-ivr-menu"),
            ("ListIVRs", "list-ivrs"),
            (
                "GetAnAgentsAssignedWorkItems",
                "get-an-agents-assigned-work-items",
            ),
            ("Ticket Comments", "ticket-comments"),
            ("SLA Policies", "sla-policies"),
            ("Talk IVR Menus", "talk-ivr-menus"),
        ] {
            assert_eq!(kebab(id), expected, "{id}");
        }
    }

    #[test]
    fn option_names_drop_brackets_underscores_and_dots() {
        for (param, expected) in [
            ("page[size]", "page-size"),
            ("filter[source_type]", "filter-source-type"),
            ("ids[]", "ids"),
            ("sort_by", "sort-by"),
            ("a.b__c", "a-b-c"),
            ("Include", "include"),
        ] {
            assert_eq!(option_name(param), expected, "{param}");
        }
    }

    #[test]
    fn the_whole_tree_is_valid_and_names_are_unique() {
        tree().debug_assert();

        let reserved: Vec<String> = crate::Cli::command()
            .get_subcommands()
            .map(|c| c.get_name().to_string())
            .collect();
        let mut group_names = HashSet::new();
        let mut op_names = HashSet::new();
        let mut seen_groups = HashSet::new();
        for op in catalog::operations() {
            let group = kebab(&op.group);
            if seen_groups.insert(&op.group) {
                assert!(group_names.insert(group.clone()), "duplicate group {group}");
                assert!(!reserved.contains(&group), "group {group} clashes");
            }
            let name = kebab(&op.id);
            assert!(op_names.insert((group, name.clone())), "duplicate {name}");
        }
    }

    #[test]
    fn colliding_parameters_are_left_to_param() {
        let op: Operation = serde_json::from_value(json!({
            "id": "X", "group": "G", "method": "POST", "summary": "s", "path": "/api/v2/x",
            "params": [
                {"name": "data", "in": "query"},
                {"name": "help", "in": "query"},
                {"name": "page[size]", "in": "query"},
                {"name": "page_size", "in": "query"},
                {"name": "[]", "in": "query"},
            ],
        }))
        .unwrap();
        let names: Vec<String> = query_options(&op).into_iter().map(|(_, n)| n).collect();
        assert_eq!(names, ["page-size"]);
        operation_command(&op).debug_assert();
    }

    #[test]
    fn show_ticket_takes_a_positional_and_options() {
        let m = parse(&["tickets", "show-ticket", "1", "--include", "users"]).unwrap();
        let (op, om) = selected(&m).unwrap();
        assert_eq!(op.id, "ShowTicket");
        assert_eq!(
            collect_args(op, om).unwrap(),
            json!({"ticket_id": "1", "include": "users"})
                .as_object()
                .unwrap()
                .clone()
        );
    }

    #[test]
    fn repeated_options_become_arrays_and_objects() {
        let m = parse(&[
            "audit-logs",
            "list-audit-logs",
            "--ids",
            "1",
            "--ids",
            "2",
            "--page",
            "size=10",
            "--page",
            "after=x",
            "--filter-source-type",
            "user",
        ])
        .unwrap();
        let (op, om) = selected(&m).unwrap();
        let args = collect_args(op, om).unwrap();
        assert_eq!(
            Value::Object(args),
            json!({
                "ids[]": ["1", "2"],
                "page": {"size": "10", "after": "x"},
                "filter[source_type]": "user",
            })
        );
    }

    #[test]
    fn an_object_option_needs_key_value_pairs_once_one_is_given() {
        let m = parse(&[
            "audit-logs",
            "list-audit-logs",
            "--page",
            "size=10",
            "--page",
            "x",
        ])
        .unwrap();
        let (op, om) = selected(&m).unwrap();
        assert!(collect_args(op, om).is_err());
    }

    #[test]
    fn a_required_body_is_required() {
        let err = parse(&["tickets", "create-ticket"]).unwrap_err();
        assert!(err.to_string().contains("--data"), "{err}");
        assert!(parse(&["tickets", "create-ticket", "-d", "{}"]).is_ok());
        // GET operations take no body.
        assert!(parse(&["tickets", "show-ticket", "1", "-d", "{}"]).is_err());
    }

    #[test]
    fn param_cannot_set_a_path_parameter() {
        let m = parse(&["tickets", "show-ticket", "1", "-p", "ticket_id=5"]).unwrap();
        let (op, om) = selected(&m).unwrap();
        let err = collect_args(op, om).unwrap_err();
        assert!(
            err.to_string().contains("ticket_id is a path parameter"),
            "{err}"
        );
    }

    #[test]
    fn non_catalog_commands_are_not_selected() {
        let m = parse(&["token"]).unwrap();
        assert!(selected(&m).is_none());
    }

    #[tokio::test]
    async fn get_sends_path_and_query() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/tickets/1"))
            .and(query_param("include", "users"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ticket": {"id": 1}})))
            .expect(1)
            .mount(&server)
            .await;
        let m = parse(&["tickets", "show-ticket", "1", "--include", "users"]).unwrap();
        let (op, om) = selected(&m).unwrap();
        let got = call(&client(&server), op, om).await.unwrap();
        assert_eq!(got, json!({"ticket": {"id": 1}}));
    }

    #[tokio::test]
    async fn post_sends_the_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/tickets"))
            .and(body_json(json!({"ticket": {"subject": "Hi"}})))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"ticket": {"id": 2}})))
            .expect(1)
            .mount(&server)
            .await;
        let m = parse(&[
            "tickets",
            "create-ticket",
            "-d",
            r#"{"ticket":{"subject":"Hi"}}"#,
        ])
        .unwrap();
        let (op, om) = selected(&m).unwrap();
        let got = call(&client(&server), op, om).await.unwrap();
        assert_eq!(got, json!({"ticket": {"id": 2}}));
    }

    #[tokio::test]
    async fn param_pairs_arrive_as_query_parameters() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/tickets"))
            .and(query_param("brand_id", "7"))
            .and(query_param("page[size]", "5"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"tickets": []})))
            .expect(1)
            .mount(&server)
            .await;
        let m = parse(&[
            "tickets",
            "list-tickets",
            "-p",
            "brand_id=7",
            "--param",
            "page[size]=5",
        ])
        .unwrap();
        let (op, om) = selected(&m).unwrap();
        call(&client(&server), op, om).await.unwrap();
    }
}
