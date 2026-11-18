//! KDE LGPL-2.1-or-later wire protocol; generated at build from vendored v6 XML.
//! The generated wire conversion uses scanner-maintained unsafe internals.
#![allow(unsafe_code)]
#![allow(
    dead_code,
    unused_imports,
    unused_unsafe,
    non_upper_case_globals,
    non_camel_case_types,
    clippy::all
)]
use wayland_client;
use wayland_client::protocol::*;
pub mod __interfaces {
    use wayland_client::protocol::__interfaces::*;
    wayland_scanner::generate_interfaces!("src/computer_fake_input.xml");
}
use self::__interfaces::*;
wayland_scanner::generate_client_code!("src/computer_fake_input.xml");
