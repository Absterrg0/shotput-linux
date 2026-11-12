//! A widget that hands its snapshot to a closure, so the editor can draw the screenshot as a
//! GPU texture and its marks as render nodes without any intermediate surfaces.

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use std::cell::RefCell;

type DrawFn = Box<dyn Fn(&gtk::Snapshot, f64, f64)>;
type ResizeFn = Box<dyn Fn(i32, i32)>;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Canvas {
        pub draw: RefCell<Option<DrawFn>>,
        pub resized: RefCell<Option<ResizeFn>>,
        pub last_size: std::cell::Cell<(i32, i32)>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Canvas {
        const NAME: &'static str = "ShotputCanvas";
        type Type = super::Canvas;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Canvas {}

    impl WidgetImpl for Canvas {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            if let Some(draw) = self.draw.borrow().as_ref() {
                draw(snapshot, widget.width() as f64, widget.height() as f64);
            }
        }

        fn measure(&self, _orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            (1, 1, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            if self.last_size.get() != (width, height) {
                self.last_size.set((width, height));
                if let Some(resized) = self.resized.borrow().as_ref() {
                    resized(width, height);
                }
            }
        }
    }
}

glib::wrapper! {
    pub struct Canvas(ObjectSubclass<imp::Canvas>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Canvas {
    pub fn new() -> Canvas {
        let canvas: Canvas = glib::Object::new();
        canvas.set_hexpand(true);
        canvas.set_vexpand(true);
        canvas.set_focusable(true);
        canvas.set_can_focus(true);
        canvas.set_overflow(gtk::Overflow::Hidden);
        canvas
    }

    pub fn set_draw(&self, f: impl Fn(&gtk::Snapshot, f64, f64) + 'static) {
        *self.imp().draw.borrow_mut() = Some(Box::new(f));
    }

    pub fn set_resized(&self, f: impl Fn(i32, i32) + 'static) {
        *self.imp().resized.borrow_mut() = Some(Box::new(f));
    }
}
