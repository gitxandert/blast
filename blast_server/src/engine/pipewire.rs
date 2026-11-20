//! PipeWire routing, when the output is a sound-server device (`default`,
//! `pipewire`, `pulse`): list the sinks (speakers, USB interfaces, HDMI,
//! Bluetooth) and move just this process's stream to one, without touching
//! the system default and without reopening the stream. Uses the `pw-dump`
//! and `pw-metadata` tools, so there is no libpipewire dependency.

use serde::Serialize;
use serde_json::Value;
use std::process::Command;

#[derive(Debug, Clone, Serialize)]
pub struct Sink {
    pub id: u64,
    pub name: String,
    pub description: String,
    /// Our stream is linked to it now.
    pub current: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Routing {
    pub available: bool,
    /// Our stream's node, when the output goes through PipeWire.
    pub stream_node: Option<u64>,
    pub sinks: Vec<Sink>,
    pub note: Option<String>,
}

fn dump() -> Result<Vec<Value>, String> {
    let out = Command::new("pw-dump")
        .arg("--no-colors")
        .output()
        .map_err(|e| format!("pw-dump: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "pw-dump failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    serde_json::from_slice::<Vec<Value>>(&out.stdout).map_err(|e| format!("pw-dump output: {e}"))
}

fn props(v: &Value) -> Option<&serde_json::Map<String, Value>> {
    v.get("info")?.get("props")?.as_object()
}

fn prop_str<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    props(v)?.get(key)?.as_str()
}

fn prop_u64(v: &Value, key: &str) -> Option<u64> {
    let p = props(v)?.get(key)?;
    p.as_u64().or_else(|| p.as_str()?.parse().ok())
}

/// The sinks and where our stream goes.
pub fn routing() -> Routing {
    let objects = match dump() {
        Ok(o) => o,
        Err(e) => {
            return Routing {
                note: Some(e),
                ..Routing::default()
            };
        }
    };
    let pid = u64::from(std::process::id());
    let is = |v: &Value, ty: &str| v.get("type").and_then(Value::as_str) == Some(ty);
    // the pid is on the client (the ALSA plugin's connection), not the node
    let ours: Vec<u64> = objects
        .iter()
        .filter(|v| is(v, "PipeWire:Interface:Client"))
        .filter(|v| prop_u64(v, "application.process.id") == Some(pid))
        .filter_map(|v| v.get("id")?.as_u64())
        .collect();
    let stream_node = objects
        .iter()
        .filter(|v| is(v, "PipeWire:Interface:Node"))
        .find(|v| {
            prop_str(v, "media.class") == Some("Stream/Output/Audio")
                && (prop_u64(v, "application.process.id") == Some(pid)
                    || prop_u64(v, "client.id").is_some_and(|c| ours.contains(&c)))
        })
        .and_then(|v| v.get("id")?.as_u64());
    let linked: Vec<u64> = objects
        .iter()
        .filter(|v| is(v, "PipeWire:Interface:Link"))
        .filter_map(|v| {
            let info = v.get("info")?;
            (Some(info.get("output-node-id")?.as_u64()?) == stream_node)
                .then(|| info.get("input-node-id")?.as_u64())?
        })
        .collect();
    let sinks = objects
        .iter()
        .filter(|v| is(v, "PipeWire:Interface:Node"))
        .filter(|v| prop_str(v, "media.class") == Some("Audio/Sink"))
        .filter_map(|v| {
            let id = v.get("id")?.as_u64()?;
            Some(Sink {
                id,
                name: prop_str(v, "node.name")?.to_string(),
                description: prop_str(v, "node.description")
                    .or_else(|| prop_str(v, "node.nick"))
                    .unwrap_or("?")
                    .to_string(),
                current: linked.contains(&id),
            })
        })
        .collect();
    Routing {
        available: true,
        note: stream_node.is_none().then(|| {
            "the output isn't a PipeWire stream (pick default, pipewire or pulse)".to_string()
        }),
        stream_node,
        sinks,
    }
}

/// Moves our stream to the sink named `sink` (its `node.name`).
pub fn route(sink: &str) -> Result<(), String> {
    let r = routing();
    let node = r.stream_node.ok_or_else(|| {
        r.note
            .clone()
            .unwrap_or_else(|| "no PipeWire stream to move".into())
    })?;
    let target = r
        .sinks
        .iter()
        .find(|s| s.name == sink)
        .ok_or_else(|| format!("no sink named '{sink}'"))?;
    // WirePlumber 0.5 reads target.object; 0.4 also target.node
    for (key, value) in [
        ("target.object", target.name.clone()),
        ("target.node", target.id.to_string()),
    ] {
        let out = Command::new("pw-metadata")
            .args(["-n", "default", &node.to_string(), key, &value])
            .output()
            .map_err(|e| format!("pw-metadata: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "pw-metadata failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
    }
    Ok(())
}
