use std::fmt::Display;

use ratatui::{
    layout::{Constraint, Rect},
    macros::{constraint, constraints},
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

pub trait TableRow {
    fn constraints(&self) -> Vec<Constraint>;
    fn title_row(&self) -> Row<'static>;
    fn value_row<'a>(&'a self) -> Row<'a>;
}

impl TableRow for App {
    fn constraints(&self) -> Vec<Constraint> {
        constraints![==100%].to_vec()
    }

    fn title_row(&self) -> Row<'static> {
        Row::new(["name"])
    }

    fn value_row<'a>(&'a self) -> Row<'a> {
        Row::new([self.metadata.name.as_str()])
    }
}

impl TableRow for Disk {
    fn constraints(&self) -> Vec<Constraint> {
        constraints![==20%, ==10%, ==30%].to_vec()
    }

    fn title_row(&self) -> Row<'static> {
        Row::new(["name", "size", "volume handle"])
    }

    fn value_row<'a>(&'a self) -> Row<'a> {
        Row::new([
            self.metadata.name.to_string(),
            format!("{}Gb", self.spec.size),
            fmt_option(self.spec.volume_handle.as_ref()),
        ])
    }
}

impl TableRow for Route {
    fn constraints(&self) -> Vec<Constraint> {
        constraints![==25%; 4].to_vec()
    }

    fn title_row(&self) -> Row<'static> {
        Row::new(["name", "hostname", "rules", "timeout"])
    }

    fn value_row<'a>(&'a self) -> Row<'a> {
        Row::new([
            self.metadata.name.clone(),
            self.spec.hostname.clone(),
            fmt_option(self.spec.domains.as_deref().map(fmt_vec)),
            fmt_option(self.spec.timeout.map(|timeout| format!("{timeout}s"))),
        ])
    }
}

impl TableRow for Resource {
    fn constraints(&self) -> Vec<Constraint> {
        match self {
            Resource::App(app) => app.constraints(),
            Resource::Disk(disk) => disk.constraints(),
            Resource::Route(route) => route.constraints(),
        }
    }

    fn title_row(&self) -> Row<'static> {
        match self {
            Resource::App(app) => app.title_row(),
            Resource::Disk(disk) => disk.title_row(),
            Resource::Route(route) => route.title_row(),
        }
    }

    fn value_row<'a>(&'a self) -> Row<'a> {
        match self {
            Resource::App(app) => app.value_row(),
            Resource::Disk(disk) => disk.value_row(),
            Resource::Route(route) => route.value_row(),
        }
    }
}

pub struct ResourceTable<'a, T: TableRow>(pub &'a [T]);

impl<'a, T: TableRow> Clone for ResourceTable<'a, T> {
    fn clone(&self) -> Self {
        Self(self.0)
    }
}

impl<'a, T: TableRow> Widget for ResourceTable<'a, T> {
    fn render(self, area: ratatui::prelude::Rect, buf: &mut ratatui::prelude::Buffer)
    where
        Self: Sized,
    {
        let Some((constraints, title_row)) = self
            .0
            .iter()
            .next()
            .map(|table| (table.constraints(), table.title_row()))
        else {
            Paragraph::new("No resources found").render(area, buf);
            return;
        };
        let header = title_row.style(
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        );
        let rows = self.0.iter().map(|item| item.value_row());

        Table::new(rows, constraints)
            .header(header)
            .render(area, buf);
    }
}
