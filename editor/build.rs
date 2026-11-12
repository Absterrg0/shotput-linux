//! Bundles the icons into a GResource with glib-compile-resources.
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let icons = PathBuf::from("data/icons");
    println!("cargo:rerun-if-changed=data/icons");
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<gresources>\n  <gresource prefix=\"/io/github/shotput/icons/scalable/actions\">\n");
    let mut names: Vec<_> = std::fs::read_dir(&icons).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().to_string()).filter(|n| n.ends_with(".svg")).collect();
    names.sort();
    for name in names {
        xml += &format!("    <file preprocess=\"xml-stripblanks\">{name}</file>\n");
    }
    xml += "  </gresource>\n</gresources>\n";
    let manifest = out.join("shotput.gresource.xml");
    std::fs::write(&manifest, xml).unwrap();
    let status = Command::new("glib-compile-resources")
        .arg(format!("--sourcedir={}", icons.display()))
        .arg(format!("--target={}", out.join("shotput.gresource").display()))
        .arg(&manifest)
        .status()
        .expect("glib-compile-resources");
    assert!(status.success());
}
