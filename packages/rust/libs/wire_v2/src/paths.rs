use stockhouse_proto::UploadId;

pub const fn probe_path() -> &'static str {
    "protocol"
}

pub const fn declare_path() -> &'static str {
    "v2/declare"
}

pub fn nar_path(id: &UploadId) -> String {
    format!("v2/nar/{id}")
}

pub const fn publish_path() -> &'static str {
    "v2/publish"
}

pub const fn status_path() -> &'static str {
    "v2/status"
}

pub const fn leases_path() -> &'static str {
    "v2/leases"
}
