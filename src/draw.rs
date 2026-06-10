use crossterm::execute;
use ratatui::{
    Terminal, TerminalOptions, Viewport,
    backend::{CrosstermBackend, TestBackend},
    layout::{Constraint, Rect},
    style::{Color, Modifier, Style},
    symbols::border,
    widgets::{Block, Borders, Row, Table, Widget},
};
use std::io::{self, Write};

// Because we're rendering inline we have to do a pre-render with a dummy backend
// this way we can scan the output buffer to see the actual rendered size
// this is pretty janky but since we're only using it to render static tables this isn't a big deal
fn compute_widget_height(widget: impl Widget) -> io::Result<u16> {
    let (width, _) = crossterm::terminal::size()?;
    let backend = TestBackend::new(width, 200);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|frame| {
        frame.render_widget(widget, frame.area());
    });

    let buf = term.backend().buffer();
    let height = (0..buf.area.height)
        .rev()
        .find(|&row| (0..buf.area.width).any(|col| buf[(col, row)].symbol() != " "))
        .map(|row| row + 1)
        .unwrap_or(1);

    Ok(height)
}

pub fn render_inline(widget: impl Widget + Clone) -> io::Result<()> {
    let mut buf: Vec<u8> = Vec::new();

    {
        let border_margin = 2;
        let height = compute_widget_height(widget.clone())?;
        let viewport = Viewport::Inline(height + border_margin); 
        let term_opts = TerminalOptions { viewport };
        let backend = CrosstermBackend::new(&mut buf);
        let mut term = Terminal::with_options(backend, term_opts)?;

        term.draw(|frame| {
            let block = Block::bordered().border_set(border::EMPTY);
            let inner = block.inner(frame.area());
            frame.render_widget(block, frame.area());
            frame.render_widget(widget, inner);
        })?;
    }

    let stdout = io::stdout();
    let mut guard = stdout.lock();
    guard.write_all(&buf)?;
    guard.write_all(&[b'\n'])?;

    Ok(())
}
