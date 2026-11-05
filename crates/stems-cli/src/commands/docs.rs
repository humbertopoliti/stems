//! `stems docs [<topic>] [--json]` (50): the pages of `docs/`, embedded in
//! the binary (`crate::docs::PAGES`). Without a topic, lists them; with
//! one, prints its Markdown. Needs no workspace and no daemon.

use serde_json::json;
use stems_core::Error;

use crate::cli::DocsArgs;
use crate::docs::{PAGES, page, title, topic_name};
use crate::output::CommandOutput;

/// Where the pages are published.
pub const ONLINE: &str = "https://github.com/humbertopoliti/stems/blob/main/docs";

/// Run `stems docs`.
pub fn run(args: &DocsArgs) -> CommandOutput {
    match args.topic.as_deref() {
        None => list(),
        Some(t) => show(t),
    }
}

fn entry(file: &str, text: &str) -> serde_json::Value {
    json!({
        "topic": topic_name(file),
        "title": title(text),
        "path": format!("docs/{file}.md"),
        "url": format!("{ONLINE}/{file}.md"),
    })
}

fn list() -> CommandOutput {
    let width = PAGES
        .iter()
        .map(|(f, _)| topic_name(f).len())
        .max()
        .unwrap_or(0);
    let mut human = String::new();
    for (file, text) in PAGES {
        human.push_str(&format!("{:<width$}  {}\n", topic_name(file), title(text)));
    }
    human.push_str(
        "\n`stems docs <topic>` prints a page (Markdown, as of this build); \
         `stems features` lists every capability with its page.\n",
    );
    let topics: Vec<_> = PAGES.iter().map(|(f, t)| entry(f, t)).collect();
    CommandOutput::data(json!({ "topics": topics })).with_human(human)
}

fn show(topic: &str) -> CommandOutput {
    let Some(text) = page(topic) else {
        let names: Vec<&str> = PAGES.iter().map(|(f, _)| topic_name(f)).collect();
        return CommandOutput::failed(Error::usage(
            format!("no documentation topic `{topic}`"),
            format!("topics: {} (`stems docs` lists them)", names.join(", ")),
        ));
    };
    let file = PAGES
        .iter()
        .find(|(_, t)| std::ptr::eq(*t, text))
        .map_or(topic, |(f, _)| *f);
    let mut data = entry(file, text);
    data["markdown"] = json!(text);
    CommandOutput::data(data).with_human(text)
}
