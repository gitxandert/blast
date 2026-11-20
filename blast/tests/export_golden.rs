//! Golden hashes of `--export` renders (PKc). Each script under
//! `tests/export_scripts/` is rendered with seed 0 against a synthetic asset (so no
//! fixture is needed), and the SHA-256 of the WAV is compared with
//! `tests/golden/<script>.sha256`. A render that changes on purpose is
//! blessed again with `BLAST_BLESS=1 cargo test --test export_golden`.

use std::path::{Path, PathBuf};

use blast::export::render::{ExportOpts, render_script};

// ---- SHA-256 (FIPS 180-4); no crate for a test-only hash ----

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

fn sha256(data: &[u8]) -> String {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [
                t1.wrapping_add(t2),
                v[0],
                v[1],
                v[2],
                v[3].wrapping_add(t1),
                v[4],
                v[5],
                v[6],
            ];
        }
        for (a, b) in h.iter_mut().zip(v) {
            *a = a.wrapping_add(b);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

#[test]
fn sha256_known_answers() {
    assert_eq!(
        sha256(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

// ---- the renders ----

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

// a folder with one deterministic noise asset, `kick.wav`
fn assets(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("blast-golden-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 48_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(dir.join("kick.wav"), spec).unwrap();
    let mut x: u32 = 12345;
    for i in 0..24_000u32 {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        // a decaying burst
        let env = 1.0 - i as f32 / 24_000.0;
        w.write_sample((((x >> 16) as i16) as f32 / 4.0 * env) as i16)
            .unwrap();
    }
    w.finalize().unwrap();
    dir
}

fn golden(script: &str) {
    let dir = assets(script);
    let out = dir.join("out.wav");
    let opts = ExportOpts {
        assets_dir: dir.clone(),
        sources_dir: dir.join("sources"),
        script: crate_dir().join(format!("tests/export_scripts/{script}.blast")),
        out: Some(out.clone()),
        length: None,
        bits: 32,
        rate: Some(48_000),
        seed: 0,
        progress: false,
    };
    render_script(&opts).unwrap();
    let hash = sha256(&std::fs::read(&out).unwrap());
    let _ = std::fs::remove_dir_all(&dir);

    let file: &Path = &crate_dir().join(format!("tests/golden/{script}.sha256"));
    if std::env::var_os("BLAST_BLESS").is_some() {
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, format!("{hash}\n")).unwrap();
        return;
    }
    let want = std::fs::read_to_string(file)
        .unwrap_or_else(|_| panic!("no {}: run with BLAST_BLESS=1", file.display()));
    assert_eq!(
        hash,
        want.trim(),
        "{script}: the render changed; if that is intended, run with BLAST_BLESS=1"
    );
}

#[test]
fn golden_pulse() {
    golden("pulse");
}

#[test]
fn golden_swell() {
    golden("swell");
}
