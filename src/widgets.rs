use std::fmt::Display;

use ratatui::{
    layout::{Constraint, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Paragraph, Row, Table, Widget},
};

use brainpod_core::resource::*;

fn fmt_option<T: Display>(opt: Option<T>) -> String {
    match opt {
        Some(val) => val.to_string(),
        None => "None".into(),
    }
}

fn fmt_vec<T: Display>(vec: &[T]) -> String {
    let mut buf = String::new();
    buf.push('[');
    for (idx, item) in vec.iter().enumerate() {
        buf.push('"');
        buf.push_str(&item.to_string());
        buf.push('"');
        if idx != vec.len() - 1 {
            buf.push(',');
            buf.push(' ');
        }
    }
    buf.push(']');
    buf
}

pub trait Tabular {
    fn field_names(&self) -> &'static [&'static str];
    fn values(&self) -> Vec<String>;
}

impl Tabular for App {
    fn field_names(&self) -> &'static [&'static str] {
        &["name"]
    }

    fn values(&self) -> Vec<String> {
        vec![self.metadata.name.clone()]
    }
}

impl Tabular for Disk {
    fn field_names(&self) -> &'static [&'static str] {
        &["name", "size", "volume handle"]
    }

    fn values(&self) -> Vec<String> {
        vec![
            self.metadata.name.clone(),
            format!("{}Gb", self.spec.size),
            fmt_option(self.spec.volume_handle.as_ref()),
        ]
    }
}

impl Tabular for Route {
    fn field_names(&self) -> &'static [&'static str] {
        &["name", "hostname", "rules", "timeout"]
    }

    fn values(&self) -> Vec<String> {
        vec![
            self.metadata.name.clone(),
            self.spec.hostname.clone(),
            // fmt_vec(&self.spec.rules),
            fmt_option(self.spec.domains.as_deref().map(fmt_vec)),
            fmt_option(self.spec.timeout.map(|timeout| format!("{timeout}s"))),
        ]
    }
}

impl Tabular for Resource {
    fn field_names(&self) -> &'static [&'static str] {
        match self {
            Resource::App(app) => app.field_names(),
            Resource::Disk(disk) => disk.field_names(),
            Resource::Route(route) => route.field_names(),
        }
    }

    fn values(&self) -> Vec<String> {
        match self {
            Resource::App(app) => app.values(),
            Resource::Disk(disk) => disk.values(),
            Resource::Route(route) => route.values(),
        }
    }
}

pub struct ResourceTable<'a, T: Tabular>(pub &'a [T]);

impl<'a, T: Tabular> Clone for ResourceTable<'a, T> {
    fn clone(&self) -> Self {
        Self(self.0)
    }
}

impl<'a, T: Tabular> Widget for ResourceTable<'a, T> {
    fn render(self, area: ratatui::prelude::Rect, buf: &mut ratatui::prelude::Buffer)
    where
        Self: Sized,
    {
        let Some(field_names) = self.0.iter().next().map(|table| table.field_names()) else {
            Paragraph::new("No resources found").render(area, buf);
            return;
        };
        let header = Row::new(field_names.to_vec()).style(
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        );

        let rows = self.0.iter().map(|item| Row::new(item.values()));
        let constraints = field_names
            .iter()
            .map(|_| Constraint::Length(100 / field_names.len() as u16));

        Table::new(rows, constraints)
            .header(header)
            .render(area, buf);
    }
}
