use divisi_runtime::assistant::actions::{execute, Reply};
use divisi_runtime::assistant::Intent;
use divisi_runtime::assistant::gate::ChatConfig;

#[test]
#[ignore = "real context not available"]
fn test_config_enable_mcp() {
    let ctx = unimplemented!();
    let cfg = ChatConfig::default();
    let reply = execute(ctx, "sess", &Intent::Config { what: "enable mcp foo".into() }, &cfg, None, &|_| None).unwrap();
    assert!(reply.text.contains("Enabled MCP foo"));
}
