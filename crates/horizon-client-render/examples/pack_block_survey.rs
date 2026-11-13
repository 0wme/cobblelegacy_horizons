//! What every block state draws from a resource root: one line per state with its number of
//! faces, how much of them is drawn (the share of texels that are not fully transparent, face by
//! face) and its sprites. Run on the prepared resources and on a pack's overlay, then compare the
//! two files: a state whose faces or drawn share drop is a block the pack makes see-through.
//! `cargo run --profile fast -p horizon-client-render --example pack_block_survey -- ROOT OUT.tsv`
use horizon_client_render::assets::Assets;
use horizon_core::position::BlockPos;
use horizon_world::registry::BlockStateId;
use std::{fmt::Write as _, path::Path};

fn main() -> horizon_client_render::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(root), Some(out)) = (args.first(), args.get(1)) else {
        eprintln!("usage: pack_block_survey ROOT OUT.tsv");
        std::process::exit(2);
    };
    let root = Path::new(root);
    let assets = Assets::load(root)?;
    let render = assets.reload(root)?;
    let side = render.sprite_size as usize;
    let pixels = render.pixels.get();
    let layer_bytes = side * side * 4;
    // The share of each layer's texels that draw something.
    let drawn: Vec<f32> = (0..render.texture_layers as usize)
        .map(|layer| {
            let texels = pixels
                .get(layer * layer_bytes..(layer + 1) * layer_bytes)
                .unwrap_or_default();
            let visible = texels.chunks_exact(4).filter(|t| t[3] != 0).count();
            visible as f32 / (side * side).max(1) as f32
        })
        .collect();
    let mut names = vec![""; render.texture_layers as usize];
    for (name, &layer) in &render.sprite_layers {
        if let Some(slot) = names.get_mut(layer as usize) {
            *slot = name.as_str();
        }
    }
    let mut text = String::new();
    let mut found = Vec::new();
    for id in 0..assets.registry.len() as u32 {
        let Some(state) = assets.registry.state(BlockStateId(id)) else {
            continue;
        };
        let bound = render
            .model_catalog
            .models_at(BlockStateId(id), BlockPos::new(0, 0, 0), &mut found)
            .is_ok();
        let faces: Vec<_> = found.iter().flat_map(|m| &m.faces).collect();
        let share = faces
            .iter()
            .map(|f| drawn.get(f.layer as usize).copied().unwrap_or(0.0))
            .sum::<f32>();
        let mut sprites: Vec<&str> = faces
            .iter()
            .map(|f| names.get(f.layer as usize).copied().unwrap_or("?"))
            .collect();
        sprites.sort_unstable();
        sprites.dedup();
        let properties: Vec<String> = state
            .properties
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        writeln!(
            text,
            "{}[{}]\t{}\t{}\t{:.2}\t{}",
            state.name,
            properties.join(","),
            if bound { "bound" } else { "unbound" },
            faces.len(),
            share,
            sprites.join(",")
        )?;
    }
    std::fs::write(out, text)?;
    println!(
        "{} states, {} layers at {side} px",
        assets.registry.len(),
        render.texture_layers
    );
    Ok(())
}
