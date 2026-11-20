use std::fs;
use std::path::Path;

use anyhow::{Context, bail};

use crate::fbx::{FbxMesh, FbxVertex};

const MAGIC: &[u8; 8] = b"MNECOOK1";
const HEADER_BYTES: usize = 16;
const VERTEX_BYTES: usize = 44;
const MAX_COOKED_VERTICES: usize = 10_000_000;

pub fn read_cooked_mesh(path: &Path) -> anyhow::Result<FbxMesh> {
    let bytes =
        fs::read(path).with_context(|| format!("could not read cooked mesh {}", path.display()))?;
    decode_cooked_mesh(&bytes).with_context(|| format!("in {}", path.display()))
}

/// Parses cooked bytes; the fuzzer exercises this directly so header/vertex
/// corruption can never panic or desynchronize the claimed vertex count.
pub fn decode_cooked_mesh(bytes: &[u8]) -> anyhow::Result<FbxMesh> {
    if bytes.len() < HEADER_BYTES || &bytes[..8] != MAGIC {
        bail!("invalid cooked mesh magic");
    }
    let count = u64::from_le_bytes(bytes[8..16].try_into()?) as usize;
    if count > MAX_COOKED_VERTICES {
        bail!("cooked mesh vertex count exceeds {MAX_COOKED_VERTICES}");
    }
    let expected = HEADER_BYTES + count * VERTEX_BYTES;
    if bytes.len() != expected {
        bail!(
            "cooked mesh size mismatch: expected {expected}, got {}",
            bytes.len()
        );
    }
    let mut vertices = Vec::with_capacity(count);
    for record in bytes[HEADER_BYTES..].chunks_exact(VERTEX_BYTES) {
        let mut values = [0_f32; 10];
        for (index, value) in values.iter_mut().enumerate() {
            *value = f32::from_le_bytes(record[index * 4..index * 4 + 4].try_into()?);
        }
        if values.iter().any(|value| !value.is_finite()) {
            bail!("cooked mesh contains non-finite vertex data");
        }
        let has_color = u32::from_le_bytes(record[40..44].try_into()?);
        if has_color > 1 {
            bail!("cooked mesh contains an invalid color flag");
        }
        vertices.push(FbxVertex {
            position: values[0..3].try_into()?,
            normal: values[3..6].try_into()?,
            color: (has_color == 1).then_some(values[6..10].try_into()?),
        });
    }
    Ok(FbxMesh { vertices })
}

/// Serializes cooked bytes without touching the filesystem so tools and the
/// fuzzer can build packages in memory.
pub fn encode_cooked_mesh(mesh: &FbxMesh) -> anyhow::Result<Vec<u8>> {
    if mesh.vertices.len() > MAX_COOKED_VERTICES {
        bail!("mesh vertex count exceeds {MAX_COOKED_VERTICES}");
    }
    let mut bytes = Vec::with_capacity(HEADER_BYTES + mesh.vertices.len() * VERTEX_BYTES);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&(mesh.vertices.len() as u64).to_le_bytes());
    for vertex in &mesh.vertices {
        let values = [
            vertex.position[0],
            vertex.position[1],
            vertex.position[2],
            vertex.normal[0],
            vertex.normal[1],
            vertex.normal[2],
            vertex.color.map_or(0.0, |color| color[0]),
            vertex.color.map_or(0.0, |color| color[1]),
            vertex.color.map_or(0.0, |color| color[2]),
            vertex.color.map_or(0.0, |color| color[3]),
        ];
        if values.iter().any(|value| !value.is_finite()) {
            bail!("cannot cook non-finite vertex data");
        }
        for value in values {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(&u32::from(vertex.color.is_some()).to_le_bytes());
    }
    Ok(bytes)
}

pub fn write_cooked_mesh(path: &Path, mesh: &FbxMesh) -> anyhow::Result<()> {
    let bytes = encode_cooked_mesh(mesh)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, bytes)?;
    Ok(())
}
