pub mod cloud;
pub mod cm;

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}
