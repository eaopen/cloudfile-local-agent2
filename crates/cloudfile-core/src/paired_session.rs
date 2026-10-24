//! V2-only browser handoff. No server-side Device/Session/Intent ledger.
//! The independent seafile:// entry is handled by legacy_seafile and only
//! opens an owned local copy or the authenticated web page.
use crate::session::Task;
use anyhow::{bail, Result};
use std::path::Path;

pub fn open(root: &Path, uri: &str) -> Result<Task> {
    if !crate::v2_edit::is_open_uri(uri) {
        bail!("旧版本地编辑会话已退出，请从 CloudFile V2 网页重新打开文件");
    }
    crate::v2_edit::open(root, uri)
}

#[cfg(test)]
mod tests {
    #[test]
    fn v1_uri_is_not_a_legacy_server_session_fallback() {
        assert!(!crate::v2_edit::is_open_uri(
            "cloudfile-open://v1/open?instance=https%3A%2F%2Fexample.com&session=x&ticket=y"
        ));
    }
}
