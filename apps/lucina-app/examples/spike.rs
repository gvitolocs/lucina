//! The screen of the toolkit comparison in `docs/research/gui.md`, in lucina-app: a filter field over a
//! 2000-row list. The same screen in Slint is `tools/slint-spike`.

use lucina_app::{App, ListState, TextEdit, Ui};

struct Spike {
    filter: TextEdit,
    list: ListState,
    rows: Vec<String>,
    shown: Vec<usize>,
}

impl App for Spike {
    type Msg = ();

    fn view(&mut self, ui: &mut Ui) {
        ui.header("lucina-app spike");
        ui.pad(14.0);
        if ui.text_field("filter", &mut self.filter).changed {
            let f = self.filter.text.to_lowercase();
            self.shown = (0..self.rows.len())
                .filter(|&i| self.rows[i].to_lowercase().contains(&f))
                .collect();
            self.list.scroll = 0;
            self.list.selected = None;
        }
        let r = ui.area();
        let (rows, shown) = (&self.rows, &self.shown);
        ui.list("rows", &mut self.list, r, shown.len(), 30.0, |ui, i, _| {
            ui.label(&rows[shown[i]]);
        });
    }
}

fn main() {
    lucina_app::run("spike", |_| {
        let rows: Vec<String> = (1..=2000)
            .map(|i| {
                format!(
                    "Row {i}: {}",
                    ["apples", "bread", "coffee", "dates", "eggs"][i % 5]
                )
            })
            .collect();
        let shown = (0..rows.len()).collect();
        Spike {
            filter: TextEdit::new().placeholder("Filter the list"),
            list: ListState::new(),
            rows,
            shown,
        }
    });
}
