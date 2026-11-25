//! The `dith capabilities` command: the self-describing JSON manifest.

use anyhow::Result;

use crate::recipe::build_capabilities;

pub(crate) fn handle_capabilities() -> Result<()> {
    let manifest = build_capabilities();
    println!("{}", serde_json::to_string_pretty(&manifest)?);
    Ok(())
}
