// ===== File: qwen36_hybrid.rs — a hybrid mixture checkpoint, read before it is run =====
//
// Qwen3.6-35B-A3B is the second family this path meets and the first that is
// not one kind of layer repeated: three of every four blocks mix tokens with a
// recurrent Gated-DeltaNet scan and the fourth with output-gated attention,
// while EVERY block feeds a mixture of 256 experts plus a shared one.
//
// This file starts at the only question that can be answered before any of that
// computes: does the checkpoint DESCRIBE itself correctly, and does the model
// refuse the parts it cannot yet compute by name. Both matter. A descriptor
// that quietly reported forty attention layers would be computed happily, and
// the answer would be fluent, wrong text.

use std::collections::HashMap;
use std::path::PathBuf;

use forge_formats::checkpoint::Checkpoint;
use forge_formats::{LayerKind, WeightRole};
use forge_kernels::HostExec;
use forge_model::dense::Dense;

fn checkpoint() -> Option<PathBuf> {
    let dir = PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../.runtime/models/qwen36-35b-a3b-mxfp4-gguf"
    ));
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "gguf"))
}

/// The file's own account of itself, held against what it contains.
///
/// Worth a test of its own because this checkpoint could not be OPENED at all
/// until now: the descriptor refused every MoE hybrid carrying a speculation
/// head, which is a property of one runtime's MTP path and not of the file. The
/// forty trunk layers were unreachable collateral, and the branch that builds
/// the mixture head was dead code no test could have reached.
#[test]
#[ignore = "wymaga checkpointu Qwen3.6-MoE"]
fn the_hybrid_checkpoint_describes_itself() {
    let Some(path) = checkpoint() else {
        eprintln!("pomijam: brak checkpointu Qwen3.6-MoE");
        return;
    };
    let ckpt = Checkpoint::open(&path).expect("otwarcie checkpointu hybrydy");
    let desc = ckpt.descriptor();
    let p = &desc.params;

    assert_eq!(desc.arch, "qwen35moe");
    // 41 blocks in the file, the last one a speculation head that the
    // autoregressive stack does not run.
    assert_eq!(desc.layers.len(), 40);
    assert_eq!(desc.layer_kinds.len(), 40);
    assert!(desc.mtp.is_some(), "głowa MTP ma być OPISANA, choć nieliczona");

    // Every fourth block mixes with attention, the rest with DeltaNet. Checked
    // as the whole sequence rather than as a count: thirty DeltaNet layers in
    // the wrong PLACES is the same count and a different model.
    let kinds: Vec<LayerKind> = desc.layer_kinds.clone();
    let expected: Vec<LayerKind> = (0..40)
        .map(|i| {
            if (i + 1) % 4 == 0 {
                LayerKind::Attention
            } else {
                LayerKind::DeltaNet
            }
        })
        .collect();
    assert_eq!(kinds, expected);

    assert_eq!(p.hidden_size, 2048);
    assert_eq!(p.n_heads, 16);
    assert_eq!(p.n_kv_heads, 2);
    // Attention heads are 256 wide, so Q is 4096 — twice the residual stream —
    // and the stored projection is twice THAT again, because it is gated.
    assert_eq!(p.head_dim, 256);
    assert!(p.attn_gated, "uwaga tej rodziny jest bramkowana");

    let moe = p.moe.as_ref().expect("mieszanka");
    assert_eq!(moe.n_experts, 256);
    assert_eq!(moe.n_experts_used, 8);
    assert_eq!(moe.moe_intermediate_size, 512);
    assert_eq!(
        moe.shared_intermediate_size, 512,
        "ekspert współdzielony liczy KAŻDY token"
    );

    let ssm = p.ssm.as_ref().expect("parametry DeltaNet");
    assert_eq!(ssm.d_conv, 4);
    assert_eq!(ssm.d_state, 128);
    // 16 key heads against 32 value heads: q and k are repeated four... two per
    // value head, which is why the recurrence cannot simply read them by index.
    assert_eq!(ssm.n_group, 16);
    assert_eq!(ssm.dt_rank, 32);
    assert_eq!(ssm.d_inner, 4096);

    // Partial rotary: the sections sum to half the rotated width, and only 64
    // of each 256-wide head turn. A full rotation here would be silent.
    let sections = p.rope_sections.expect("sekcje M-RoPE");
    assert_eq!(sections, [11, 11, 10, 0]);
    assert_eq!(sections.iter().sum::<u32>() * 2, 64);

    // Every trunk layer carries the mixture AND the shared expert; the roles a
    // layer carries are what the loader is held against.
    for (index, layer) in desc.layers.iter().enumerate() {
        for role in [
            WeightRole::FfnGateInp,
            WeightRole::FfnGateExps,
            WeightRole::FfnUpExps,
            WeightRole::FfnDownExps,
            WeightRole::FfnGateShExp,
            WeightRole::FfnUpShExp,
            WeightRole::FfnDownShExp,
            WeightRole::FfnGateInpShExp,
        ] {
            assert!(layer.contains_key(&role), "warstwa {index} bez {role:?}");
        }
    }
}

/// What this model cannot yet compute, refused BEFORE a weight is uploaded.
///
/// The executor is a factory that panics, so the test says something stronger
/// than "an error came back": the refusal happens while reading the checkpoint,
/// not after twenty gigabytes have gone to a device.
#[test]
#[ignore = "wymaga checkpointu Qwen3.6-MoE"]
fn the_parts_not_yet_computed_are_refused_by_name() {
    let Some(path) = checkpoint() else {
        eprintln!("pomijam: brak checkpointu Qwen3.6-MoE");
        return;
    };
    let loaded = Dense::load(&path, |_| -> forge_types::Result<HostExec> {
        panic!("checkpoint ma zostać odrzucony ZANIM powstanie wykonawca")
    });
    let Err(err) = loaded else {
        panic!("hybryda przeszła przez ścieżkę gęstą");
    };
    let why = format!("{err}");

    // The first layer is DeltaNet, and the roles it carries are the ones this
    // model does not read. Naming them is the whole point: "unsupported" alone
    // would not say which milestone is missing.
    for role in [
        WeightRole::SsmInProj,
        WeightRole::SsmConv1d,
        WeightRole::SsmA,
    ] {
        assert!(why.contains(&format!("{role:?}")), "{why}");
    }
    // The FIRST layer, and it is named. A refusal that pointed at layer 3 would
    // mean the loader had walked past thirty recurrent blocks without noticing.
    assert!(why.contains("warstwa 0:"), "{why}");
    // The shared expert is refused in the same breath, and it is a separate
    // milestone: it is a second feed-forward block on every token of every
    // layer, including the ten this model already knows how to mix.
    assert!(why.contains("FfnGateInpShExp"), "{why}");
}

/// The role map is the contract between a checkpoint and this model, so the
/// three things 4b still owes it are stated as a list rather than as a memory.
///
/// This runs without the checkpoint on purpose: it is the part of the
/// description that does not depend on twenty gigabytes being present.
#[test]
fn the_hybrid_owes_three_things_to_the_role_map() {
    let read: HashMap<WeightRole, ()> = forge_model::dense::required_roles()
        .iter()
        .chain(forge_model::dense::optional_roles())
        .map(|r| (*r, ()))
        .collect();
    for role in [
        // The shared expert — a second feed-forward block on every token.
        WeightRole::FfnGateShExp,
        WeightRole::FfnUpShExp,
        WeightRole::FfnDownShExp,
        WeightRole::FfnGateInpShExp,
        // The recurrent mixer.
        WeightRole::SsmInProj,
        WeightRole::SsmConv1d,
        WeightRole::SsmOut,
    ] {
        assert!(
            !read.contains_key(&role),
            "{role:?} jest już czytana, więc ta lista jest nieaktualna"
        );
    }
}
