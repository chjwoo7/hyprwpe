//! hyprwpe-gui — a picker for the hyprwpe daemon.
//!
//! A separate binary rather than a subcommand so none of GTK is linked into the
//! process that runs all session. It is also a plain client: everything it does
//! goes through the same socket the CLI uses, so it cannot reach anything
//! `hyprwpe` cannot.

mod thumbs;

use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;
use hyprwpe_core::client;
use hyprwpe_core::protocol::{Request, Response, Status, Target};
use hyprwpe_core::{config, Catalog, Kind};
use libadwaita as adw;
use libadwaita::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

const APP_ID: &str = "dev.chjwoo.hyprwpe";

const SCALINGS: [&str; 4] = ["fill", "fit", "stretch", "center"];
const KINDS: [(&str, Option<Kind>); 5] = [
    ("All", None),
    ("Images", Some(Kind::Image)),
    ("Scenes", Some(Kind::Scene)),
    ("Videos", Some(Kind::Video)),
    ("Web", Some(Kind::Web)),
];

/// What the header selections apply to the next click.
#[derive(Clone)]
struct Selection {
    output: Option<String>,
    scaling: String,
}

/// Everything the callbacks need, shared rather than threaded through.
struct App {
    catalog: Catalog,
    selection: RefCell<Selection>,
    /// Catalog indices currently in the grid, in display order.
    visible: RefCell<Vec<usize>>,
    outputs: RefCell<Vec<String>>,
}

fn main() -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run()
}

fn build_ui(gtk_app: &adw::Application) {
    let app = Rc::new(App {
        catalog: Catalog::scan(&config::default_sources()),
        selection: RefCell::new(Selection {
            output: None,
            scaling: "fill".to_string(),
        }),
        visible: RefCell::new(Vec::new()),
        outputs: RefCell::new(Vec::new()),
    });

    let window = adw::ApplicationWindow::builder()
        .application(gtk_app)
        .title("hyprwpe")
        .default_width(1000)
        .default_height(720)
        .build();

    let overlay = adw::ToastOverlay::new();
    let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);

    let header = adw::HeaderBar::new();
    let kind_drop = gtk4::DropDown::from_strings(&KINDS.map(|(label, _)| label));
    let scaling_drop = gtk4::DropDown::from_strings(&SCALINGS);
    let output_drop = gtk4::DropDown::from_strings(&["All outputs"]);
    header.pack_start(&kind_drop);
    header.pack_end(&output_drop);
    header.pack_end(&scaling_drop);
    root.append(&header);

    // Shown only when the daemon is missing. The GUI cannot do anything useful
    // without it, so say so plainly instead of failing on the first click.
    let banner = adw::Banner::builder()
        .title("No hyprwpe daemon is running. Start it with `hyprwpe daemon`.")
        .revealed(false)
        .build();
    root.append(&banner);

    // The model holds catalog indices, not widgets. GridView recycles the
    // widgets it shows, so a library of any size keeps only a screenful of
    // decoded thumbnails alive — the whole reason this is not a FlowBox.
    let model = gio::ListStore::new::<gtk4::StringObject>();
    let factory = gtk4::SignalListItemFactory::new();

    {
        // The click handler is attached once per widget here, not on every
        // bind, or a recycled tile would accumulate one controller per
        // wallpaper it ever showed. It reads the index the bind step stamps on
        // the widget, so one handler serves every wallpaper the tile displays.
        let app = app.clone();
        let overlay = overlay.clone();
        factory.connect_setup(move |_, item| {
            let item = item.downcast_ref::<gtk4::ListItem>().expect("list item");
            let child = tile_skeleton();

            let gesture = gtk4::GestureClick::new();
            let app = app.clone();
            let overlay = overlay.clone();
            let target = child.clone();
            gesture.connect_released(move |_, _, _, _| {
                let Ok(index) = target.widget_name().parse::<usize>() else {
                    return;
                };
                let Some(wallpaper) = app.catalog.wallpapers.get(index) else {
                    return;
                };
                let message = apply(wallpaper, &app);
                overlay.add_toast(adw::Toast::new(&message));
            });
            child.add_controller(gesture);

            item.set_child(Some(&child));
        });
    }

    {
        let app = app.clone();
        factory.connect_bind(move |_, item| {
            let item = item.downcast_ref::<gtk4::ListItem>().expect("list item");
            let Some(child) = item.child() else { return };
            let Some(index) = item
                .item()
                .and_downcast::<gtk4::StringObject>()
                .and_then(|s| s.string().parse::<usize>().ok())
            else {
                return;
            };
            bind_tile(&child, index, &app);
        });
    }

    factory.connect_unbind(|_, item| {
        let item = item.downcast_ref::<gtk4::ListItem>().expect("list item");
        if let Some(child) = item.child() {
            // Drop the texture as the tile scrolls away. Without this the
            // recycling buys nothing: every thumbnail ever shown would stay
            // resident for the life of the window.
            if let Some(picture) = child.first_child().and_downcast::<gtk4::Picture>() {
                picture.set_paintable(None::<&gtk4::gdk::Texture>);
            }
        }
    });

    let grid = gtk4::GridView::builder()
        .model(&gtk4::NoSelection::new(Some(model.clone())))
        .factory(&factory)
        .min_columns(2)
        .max_columns(8)
        .vexpand(true)
        .build();

    let scroller = gtk4::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .child(&grid)
        .build();
    root.append(&scroller);

    let status_label = gtk4::Label::builder()
        .xalign(0.0)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(12)
        .margin_end(12)
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .build();
    status_label.add_css_class("dim-label");
    root.append(&status_label);

    overlay.set_child(Some(&root));
    window.set_content(Some(&overlay));

    refill(&model, &app, None);

    {
        let app = app.clone();
        let model = model.clone();
        kind_drop.connect_selected_notify(move |d| {
            refill(&model, &app, KINDS[d.selected() as usize].1);
        });
    }

    {
        let app = app.clone();
        scaling_drop.connect_selected_notify(move |d| {
            app.selection.borrow_mut().scaling = SCALINGS[d.selected() as usize].to_string();
        });
    }

    {
        let app = app.clone();
        output_drop.connect_selected_notify(move |d| {
            let i = d.selected() as usize;
            let outputs = app.outputs.borrow().clone();
            app.selection.borrow_mut().output = if i == 0 {
                None
            } else {
                outputs.get(i - 1).cloned()
            };
        });
    }

    refresh_status(&app, &status_label, &banner, &output_drop);

    // Refreshed after every change rather than on a timer: a GUI that polls
    // costs CPU forever, which is the opposite of what this project is for.
    {
        let app = app.clone();
        let label = status_label.clone();
        let banner = banner.clone();
        let output_drop = output_drop.clone();
        overlay.connect_child_notify(move |_| {
            refresh_status(&app, &label, &banner, &output_drop);
        });
    }

    window.present();
}

/// Replace the grid contents with the catalog indices matching `kind`.
fn refill(model: &gio::ListStore, app: &Rc<App>, kind: Option<Kind>) {
    let indices: Vec<usize> = app
        .catalog
        .wallpapers
        .iter()
        .enumerate()
        .filter(|(_, w)| kind.is_none_or(|k| w.kind == k))
        .map(|(i, _)| i)
        .collect();

    model.remove_all();
    let objects: Vec<gtk4::StringObject> = indices
        .iter()
        .map(|i| gtk4::StringObject::new(&i.to_string()))
        .collect();
    model.extend_from_slice(&objects);
    *app.visible.borrow_mut() = indices;
}

/// An empty tile. Filled on bind and emptied on unbind, so the widget outlives
/// the wallpaper it happens to be showing.
fn tile_skeleton() -> gtk4::Widget {
    let picture = gtk4::Picture::builder()
        .content_fit(gtk4::ContentFit::Cover)
        .height_request(135)
        .build();
    picture.add_css_class("card");

    let title = gtk4::Label::builder()
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .max_width_chars(24)
        .build();

    let subtitle = gtk4::Label::new(None);
    subtitle.add_css_class("dim-label");
    subtitle.add_css_class("caption");

    let column = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    column.append(&picture);
    column.append(&title);
    column.append(&subtitle);
    column.upcast()
}

fn bind_tile(child: &gtk4::Widget, index: usize, app: &Rc<App>) {
    let Some(wallpaper) = app.catalog.wallpapers.get(index) else {
        return;
    };
    let Some(picture) = child.first_child().and_downcast::<gtk4::Picture>() else {
        return;
    };
    let Some(title) = picture.next_sibling().and_downcast::<gtk4::Label>() else {
        return;
    };
    let Some(subtitle) = title.next_sibling().and_downcast::<gtk4::Label>() else {
        return;
    };

    title.set_text(&wallpaper.title);
    subtitle.set_text(&if wallpaper.supported() {
        wallpaper.kind.as_str().to_string()
    } else {
        format!("{} · unsupported", wallpaper.kind.as_str())
    });

    picture.set_paintable(None::<&gtk4::gdk::Texture>);
    if let Some(preview) = &wallpaper.preview {
        thumbs::load_into(&picture, preview);
    }

    // Stamp the index where the click handler installed at setup can find it.
    // Every tile stays clickable, including kinds hyprwpe cannot render yet:
    // greying out ninety of a hundred wallpapers makes a working application
    // look broken, while a click that explains itself does not.
    child.set_widget_name(&index.to_string());
}

fn apply(wallpaper: &hyprwpe_core::Wallpaper, app: &Rc<App>) -> String {
    if wallpaper.kind != Kind::Image {
        return format!(
            "hyprwpe cannot render {} wallpapers yet",
            wallpaper.kind.as_str()
        );
    }

    let sel = app.selection.borrow().clone();
    let target = match &sel.output {
        Some(name) => Target::Output(name.clone()),
        None => Target::All,
    };
    let request = Request::Set {
        path: wallpaper.path.clone(),
        target,
        scaling: sel.scaling.clone(),
    };
    match client::send(&request) {
        Ok(Response::Ok) => format!("Set {}", wallpaper.title),
        Ok(Response::Error { message }) => message,
        Ok(_) => "Unexpected response from the daemon".to_string(),
        Err(e) => format!("{e}"),
    }
}

fn refresh_status(
    app: &Rc<App>,
    label: &gtk4::Label,
    banner: &adw::Banner,
    output_drop: &gtk4::DropDown,
) {
    match client::send(&Request::Status) {
        Ok(Response::Status(status)) => {
            banner.set_revealed(false);
            label.set_text(&describe(&status));

            let names: Vec<String> = status.outputs.iter().map(|o| o.name.clone()).collect();
            if *app.outputs.borrow() != names {
                let mut items = vec!["All outputs".to_string()];
                items.extend(names.iter().cloned());
                let refs: Vec<&str> = items.iter().map(|s| s.as_str()).collect();
                output_drop.set_model(Some(&gtk4::StringList::new(&refs)));
                *app.outputs.borrow_mut() = names;
            }
        }
        _ => {
            banner.set_revealed(true);
            label.set_text("Not connected to a daemon.");
        }
    }
}

fn describe(status: &Status) -> String {
    let mut parts: Vec<String> = status
        .outputs
        .iter()
        .map(|o| {
            let name = o
                .wallpaper
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_else(|| "nothing".into());
            format!("{}: {}", o.name, name)
        })
        .collect();
    if let Some(kb) = status.rss_kb {
        parts.push(format!("daemon {:.0} MB", kb as f64 / 1024.0));
    }
    parts.join("   ·   ")
}
