use super::NGEventBus;
use colored::*;
use ng_gateway_error::NGResult;
use ng_gateway_models::event::ApplicationReady;
use ng_gateway_models::EventBus;
use std::io::{self, Write};
use sys_locale::get_locale;
use term_size;

pub(super) async fn register_builtin_events(event_bus: &NGEventBus) {
    event_bus
        .register_handler::<ApplicationReady, _>(application_is_ready)
        .await;
}

struct Translations {
    welcome: String,
    version: String,
    repository: String,
    copyright_header: String,
    copyright_footer: String,
    copyright: String,
    license: String,
}

impl Translations {
    fn get_current() -> Self {
        let locale = get_locale().unwrap_or_else(|| "en-US".into());
        let is_chinese = locale.starts_with("zh");

        if is_chinese {
            Self {
                welcome: "欢迎使用 NG Gateway".into(),
                version: "当前版本:".into(),
                repository: "项目地址:".into(),
                copyright_header: "--------------------------------------版权声明--------------------------------------".into(),
                copyright_footer: "------------------------------------------------------------------------------------".into(),
                copyright: "版权所有:".into(),
                license: "开源协议:".into(),
            }
        } else {
            Self {
                welcome: "Welcome to NG Gateway".into(),
                version: "Version:".into(),
                repository: "Repository:".into(),
                copyright_header: "--------------------------------------Copyright--------------------------------------".into(),
                copyright_footer: "------------------------------------------------------------------------------------".into(),
                copyright: "Copyright:".into(),
                license: "License:".into(),
            }
        }
    }
}

fn application_is_ready(_: &ApplicationReady) -> NGResult<()> {
    colored::control::set_override(true);

    let (version, repo) = (env!("CARGO_PKG_VERSION"), env!("CARGO_PKG_REPOSITORY"));

    let t = Translations::get_current();

    let term_width = term_size::dimensions().map(|(w, _)| w).unwrap_or(80);

    let banner = format!(
        "\n{}\n{} v{version}\n{} {repo}\n\n{}\n{} {}\n{} {}\n{}\n",
        t.welcome,
        t.version,
        t.repository,
        t.copyright_header,
        t.copyright,
        "ShiyueCamus",
        t.license,
        "Apache License 2.0",
        t.copyright_footer
    );

    let colored_banner = banner
        .lines()
        .map(|line| {
            if line.contains(&t.welcome) {
                line.bright_green().bold().to_string()
            } else if line.contains(&t.version) {
                format!("{} v{}", t.version.bright_yellow(), version.bright_white())
            } else if line.contains(&t.repository) {
                format!(
                    "{}{}",
                    t.repository.bright_yellow(),
                    repo.bright_blue().underline()
                )
            } else if line.contains(&t.copyright_header) || line.contains(&t.copyright_footer) {
                line.bright_magenta().to_string()
            } else if line.contains(&t.copyright) {
                format!(
                    "{}{}",
                    t.copyright.bright_yellow(),
                    "Shiyuecamus".bright_white()
                )
            } else if line.contains(&t.license) {
                format!(
                    "{}{}",
                    t.license.bright_yellow(),
                    "Apache License 2.0".bright_white()
                )
            } else {
                line.into()
            }
        })
        .collect::<Vec<String>>()
        .join("\n");

    let get_original_width = |line: &str| -> usize {
        if line.contains(&t.welcome) {
            t.welcome.len()
        } else if line.contains(&t.version) {
            format!("{} v{version}", t.version).len()
        } else if line.contains(&t.repository) {
            format!("{} {repo}", t.repository).len()
        } else if line.contains(&t.copyright_header) || line.contains(&t.copyright_footer) {
            t.copyright_header.len()
        } else if line.contains(&t.copyright) {
            format!("{} Shiyuecamus", t.copyright).len()
        } else if line.contains(&t.license) {
            format!("{} Apache License 2.0", t.license).len()
        } else {
            line.trim_start().len()
        }
    };

    for line in colored_banner.lines() {
        let original_width = get_original_width(line);
        let padding = (term_width.saturating_sub(original_width)) / 2;
        writeln!(io::stdout(), "{}{line}", " ".repeat(padding))?;
    }

    Ok(())
}
