use std::fmt::Display;

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Rect},
    macros::constraints,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Padding, Paragraph, Row, Table, Widget},
};

use brainpod_core::pod::PodMeta;
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

impl TableRow for PodMeta {
    fn constraints(&self) -> Vec<Constraint> {
        constraints![==20%; 5].to_vec()
    }

    fn title_row(&self) -> Row<'static> {
        Row::new(["Name", "Display name", "Head", "Created at", "Owner"])
    }

    fn value_row<'a>(&'a self) -> Row<'a> {
        let head = self.head.to_string();
        let (head, _) = head.split_once("-").expect("head to be a valid uuid");
        Row::new([
            Cell::new(self.name.as_str()),
            Cell::new(fmt_option(self.display_name.as_deref())),
            Cell::new(head.to_string()),
            Cell::new(self.created_at.to_string()),
            Cell::new(self.is_owner.to_string()),
        ])
    }
}

impl TableRow for App {
    fn constraints(&self) -> Vec<Constraint> {
        constraints![==50%; 2].to_vec()
    }

    fn title_row(&self) -> Row<'static> {
        Row::new(["name"])
    }

    fn value_row<'a>(&'a self) -> Row<'a> {
        Row::new([
            Cell::new(self.metadata.name.as_str()),
            Cell::new("hi").style(Style::default().on_red()),
        ])
    }
}

impl TableRow for Disk {
    fn constraints(&self) -> Vec<Constraint> {
        constraints![==20%, ==10%, ==30%].to_vec()
    }

    fn title_row(&self) -> Row<'static> {
        Row::new(["Name", "Size", "Volume handle"])
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
        Row::new(["Name", "Hostname", "Rules", "Timeout"])
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

pub struct TableWidget<'a, T: TableRow>(pub &'a [T]);

impl<'a, T: TableRow> Clone for TableWidget<'a, T> {
    fn clone(&self) -> Self {
        Self(self.0)
    }
}

impl<'a, T: TableRow> Widget for TableWidget<'a, T> {
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

#[derive(Clone)]
pub struct PodMetaWidget<'a>(pub &'a PodMeta);

impl<'a> Widget for PodMetaWidget<'a> {
    fn render(self, area: ratatui::prelude::Rect, buf: &mut ratatui::prelude::Buffer)
    where
        Self: Sized,
    {
        let pod = self.0;
        let label = Style::default().fg(Color::DarkGray);

        let field = |label_text: &'static str, value: Span<'static>| {
            Line::from(vec![
                Span::styled(format!("{label_text:<14}"), label),
                value,
            ])
        };

        let display_name = pod.display_name.clone().unwrap_or_else(|| "—".into());
        let created_at = pod.created_at.to_rfc3339();
        let owner = if pod.is_owner {
            Span::styled("yes", Style::default().fg(Color::Green))
        } else {
            Span::styled("no", Style::default().fg(Color::Red))
        };

        let lines = vec![
            field(
                "Name",
                Span::styled(
                    pod.name.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ),
            field("Display name", Span::raw(display_name)),
            field(
                "Head",
                Span::styled(pod.head.to_string(), Style::default().fg(Color::Cyan)),
            ),
            field(
                "Created at",
                Span::styled(created_at, Style::default().fg(Color::Yellow)),
            ),
            field("Owner", owner),
        ];

        Paragraph::new(lines).render(area, buf);
    }
}
