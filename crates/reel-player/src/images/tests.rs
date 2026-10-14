use super::*;
use crate::assets::AssetOptions;
use reel_ui::resources::{ColorEncoding, ResourceData};
use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver},
    time::Duration,
};

fn loader() -> (Images, Receiver<()>) {
    configured(256 << 20, 256 << 20)
}
fn configured(resident_bytes: u64, decode_bytes: u64) -> (Images, Receiver<()>) {
    let mut images = Images::new(Assets::directory(AssetOptions {
        root: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples"),
        resident_bytes,
        decode_bytes,
        ..Default::default()
    }));
    let (send, receive) = mpsc::channel();
    images.set_waker(Arc::new(move || {
        let _ = send.send(());
    }));
    (images, receive)
}
fn wanted(sources: &[&str]) -> BTreeMap<String, Option<String>> {
    sources
        .iter()
        .map(|source| (source.to_string(), Some("main:17:9".into())))
        .collect()
}
fn ready(images: &mut Images, wake: &Receiver<()>) -> Result<(), String> {
    loop {
        if images.poll()? {
            return Ok(());
        }
        wake.recv_timeout(Duration::from_secs(10))
            .expect("decoder must wake its host");
    }
}
fn pixels(lease: &ResourceLease) -> &reel_ui::peniko::ImageData {
    let ResourceData::Image { image, encoding } = lease.data() else {
        panic!("expected image")
    };
    assert_eq!(*encoding, ColorEncoding::Srgb);
    image
}

#[test]
fn real_images_share_storage_replace_atomically_and_retire_with_frame_leases() {
    let (mut images, wake) = loader();
    images.request(wanted(&["assets/panels.png", "assets/panels.png"]));
    ready(&mut images, &wake).unwrap();
    images.commit();
    let submitted_frame = images.loaded["assets/panels.png"].clone();
    let panel = pixels(&submitted_frame);
    assert_eq!([panel.width, panel.height], [160, 90]);
    assert_eq!(&panel.data.as_ref()[0..4], &[224, 64, 48, 255]);
    assert_eq!(&panel.data.as_ref()[80 * 4..81 * 4], &[32, 112, 224, 128]);
    images.request(wanted(&["assets/panels.png"]));
    ready(&mut images, &wake).unwrap();
    assert_eq!(images.loads, 1);
    assert_eq!(
        pixels(&images.loaded["assets/panels.png"]).data.id(),
        panel.data.id()
    );
    assert_eq!(images.pool.usage().bytes, 160 * 90 * 4);

    images.request(wanted(&["assets/sun.jpg"]));
    assert!(!images.poll().unwrap());
    assert!(
        !submitted_frame.is_retired(),
        "old complete scene remains publishable while loading"
    );
    assert!(images.loaded.contains_key("assets/panels.png"));
    ready(&mut images, &wake).unwrap();
    assert!(
        !submitted_frame.is_retired(),
        "readiness alone must not retire the visible scene"
    );
    let sun = pixels(&images.loaded["assets/sun.jpg"]);
    assert_eq!([sun.width, sun.height], [96, 96]);
    for (offset, expected) in [
        (0, [20_i16, 48, 88, 255]),
        ((48 * 96 + 48) * 4, [255, 200, 48, 255]),
    ] {
        let actual = &sun.data.as_ref()[offset..offset + 4];
        assert!(
            actual
                .iter()
                .zip(expected)
                .all(|(actual, expected)| (i16::from(*actual) - expected).abs() <= 8),
            "JPEG pixel={actual:?}, expected={expected:?}"
        );
    }
    images.commit();
    assert_eq!(images.loads, 2);
    assert!(submitted_frame.is_retired());
    assert!(!images.loaded.contains_key("assets/panels.png"));
    assert_eq!(
        &pixels(&submitted_frame).data.as_ref()[0..4],
        &[224, 64, 48, 255]
    );
    images.close();
    assert_eq!(images.pool.usage().bytes, 160 * 90 * 4);
    drop(submitted_frame);
    assert_eq!(images.pool.usage().bytes, 0);
    assert_eq!(images.work.usage().bytes, 0);
    eprintln!(
        "decodes={}, resident={:?}, decoding={:?}",
        images.loads,
        images.pool.usage(),
        images.work.usage()
    );
}

#[test]
fn worker_errors_keep_constructor_location_and_cancelled_completions_cannot_resurrect() {
    for (source, resident, decode, expected) in [
        ("assets/missing.png", 1 << 20, 1 << 20, "image"),
        (
            "author_images.aside",
            1 << 20,
            1 << 20,
            "expected PNG or JPEG",
        ),
        ("assets/panels.png", 1, 1 << 20, "resident budget"),
        ("assets/panels.png", 1 << 20, 1, "limit"),
    ] {
        let (mut images, wake) = configured(resident, decode);
        images.request(wanted(&[source]));
        let error = ready(&mut images, &wake).unwrap_err();
        assert!(
            error.contains("main:17:9") && error.contains(source) && error.contains(expected),
            "{error}"
        );
        assert_eq!(images.pool.usage().bytes, 0);
        assert_eq!(images.work.usage().bytes, 0);
    }
    let (mut images, wake) = loader();
    images.request(wanted(&["assets/panels.png"]));
    assert!(!images.poll().unwrap());
    // Leave a successful decoded lease queued, then withdraw its request before
    // polling. This tests an actual late result, rather than only early IO cancellation.
    loop {
        wake.recv_timeout(Duration::from_secs(10)).unwrap();
        if images.pool.usage().bytes == 160 * 90 * 4 && images.work.usage().bytes == 0 {
            break;
        }
        assert!(!images.poll().unwrap());
    }
    assert_eq!(images.pool.usage().bytes, 160 * 90 * 4);
    assert_eq!(images.work.usage().bytes, 0);
    images.request(BTreeMap::new());
    assert!(images.poll().unwrap());
    assert!(images.loaded.is_empty());
    assert_eq!(images.pool.usage().bytes, 0);
    images.request(wanted(&["assets/sun.jpg"]));
    assert!(!images.poll().unwrap());
    let resident = images.pool.clone();
    let working = images.work.clone();
    images.close();
    // Exit notification follows receiver destruction and dropping any late result.
    loop {
        wake.recv_timeout(Duration::from_secs(10)).unwrap();
        if resident.usage().bytes == 0 && working.usage().bytes == 0 {
            break;
        }
    }
    assert!(!images.poll().unwrap());
    assert!(images.loaded.is_empty());
    assert_eq!(resident.usage().bytes, 0);
    assert_eq!(working.usage().bytes, 0);
}
