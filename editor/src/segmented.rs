//! A segmented choice of linked toggle buttons. libadwaita 1.7 has AdwToggleGroup; this works on
//! the libadwaita 1.5 that Ubuntu 24.04 ships.

use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone)]
pub struct Segmented {
    pub widget: gtk::Box,
    buttons: Rc<Vec<(String, gtk::ToggleButton)>>,
    handlers: Rc<RefCell<Vec<Box<dyn Fn()>>>>,
}

impl Segmented {
    pub fn new(items: &[(&str, &str)]) -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        widget.add_css_class("linked");
        widget.set_homogeneous(true);
        let handlers: Rc<RefCell<Vec<Box<dyn Fn()>>>> = Rc::new(RefCell::new(Vec::new()));
        let mut buttons = Vec::new();
        for (name, label) in items {
            let button = gtk::ToggleButton::with_label(label);
            button.set_focus_on_click(false);
            if let Some((_, first)) = buttons.first() {
                button.set_group(Some(first));
            }
            let handlers = handlers.clone();
            button.connect_toggled(move |b| {
                if b.is_active() {
                    for h in handlers.borrow().iter() {
                        h();
                    }
                }
            });
            widget.append(&button);
            buttons.push((name.to_string(), button));
        }
        Segmented { widget, buttons: Rc::new(buttons), handlers }
    }

    pub fn active_name(&self) -> Option<String> {
        self.buttons.iter().find(|(_, b)| b.is_active()).map(|(n, _)| n.clone())
    }

    pub fn set_active_name(&self, name: Option<&str>) {
        if let Some((_, b)) = self.buttons.iter().find(|(n, _)| Some(n.as_str()) == name) {
            b.set_active(true);
        }
    }

    pub fn connect_active_name_notify(&self, f: impl Fn(&Self) + 'static) {
        let this = self.clone();
        self.handlers.borrow_mut().push(Box::new(move || f(&this)));
    }
}
