//! A delayed image handoff cannot reuse a pre-load dialogue receipt.
use super::*;

#[test]
fn delayed_image_replacement_rejects_a_preload_receipt_after_loading() {
    let source = r#"fn main() {
        let picture = image("assets/panels.png", width: 160, height: 90);
        stage.replace(picture);
        spawn(|| {
            await run(delay(1));
            picture.source = "assets/sun.jpg";
        });
        narrator("The same unfinished dialogue stays active during the image replacement.");
    }"#;
    let (mut driver, mut bridge, root, wake) =
        fixture_with_source(source, crate::assets::tests::memory_assets());
    drive(&mut driver);
    settle(&mut driver, &mut bridge, root, &wake);
    let first = frame(&mut driver, &mut bridge, root, 1);
    assert_eq!(driver.ui().images.loads, 1);
    assert!(driver.ui().dialogue_needs_time());
    driver.ui_mut().advance_dialogue(0, 10.).unwrap();

    // Retain an actual recorded candidate, including its old image and geometry,
    // without accepting it yet. This models a backend waiting on a GPU fence.
    let (old_source, old_receipt) = driver
        .record(&mut bridge, root, FrameId(2))
        .unwrap()
        .expect("pre-replacement candidate");
    assert!(old_receipt.current);
    let old_pixels = image_pixel_ids(&first);
    assert_eq!(old_pixels.len(), 1);

    while driver.advance_time(1., 4096).unwrap().pending {}
    drive(&mut driver);
    driver.sync(&mut bridge, root).unwrap();
    assert!(driver.ui().images_pending());
    assert!(!driver.ui().dialogue_needs_time());
    assert!(
        !driver
            .ui_mut()
            .advance_dialogue(5_000_000_000, 10.)
            .unwrap()
    );
    assert!(
        driver
            .record(&mut bridge, root, FrameId(3))
            .unwrap()
            .is_none()
    );
    settle(&mut driver, &mut bridge, root, &wake);
    assert!(!driver.ui().images_pending());
    assert_eq!(driver.ui().images.loads, 2);
    assert!(driver.ui().images.is_required("assets/sun.jpg"));
    assert_eq!(image_pixel_ids(&first), old_pixels);

    let current = driver.ui().projection_stamp();
    assert_eq!(old_receipt.stamp.interaction, current.interaction);
    assert_eq!(
        old_receipt.stamp.dialogue_revision,
        current.dialogue_revision
    );
    assert!(!old_receipt.stamp.dialogue_complete && !current.dialogue_complete);
    assert_eq!(
        bridge.resolve(PresentationNotice::new(
            old_source.key(),
            SurfaceEpoch(1),
            PresentationOutcome::Accepted(FrameId(2)),
        )),
        NoticeResult::Accepted,
        "the delayed candidate really becomes the bridge's accepted geometry"
    );
    driver.presented(&bridge, &old_receipt).unwrap();
    assert!(driver.ui().binding.borrow().control_accepted.is_some());
    assert!(
        !driver.ui().dialogue_needs_time(),
        "a pre-load receipt must not resume reading after the new image has loaded"
    );
    assert!(
        !driver
            .ui_mut()
            .advance_dialogue(10_000_000_000, 10.)
            .unwrap()
    );

    let replaced = frame(&mut driver, &mut bridge, root, 3);
    assert_ne!(image_pixel_ids(&replaced), old_pixels);
    assert!(driver.ui().dialogue_needs_time());
    assert!(
        !driver
            .ui_mut()
            .advance_dialogue(20_000_000_000, 10.)
            .unwrap()
    );
    assert!(
        driver
            .ui_mut()
            .advance_dialogue(20_100_000_000, 10.)
            .unwrap()
    );
    let Some(InteractionView::Dialogue(dialogue)) = driver.ui().session().view().interaction else {
        panic!("the original dialogue must still be active")
    };
    assert_eq!(Some(dialogue.id()), current.interaction);
    assert_eq!(dialogue.reveal().revealed_count, 1);
}
