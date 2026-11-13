use render::{
    COLOR_FORMAT, Camera, FLAG_DELETE, FLAG_HIDDEN, FLAG_HIDDEN_GHOST, FLAG_PICK_HIDDEN, Gpu,
    Picker, Renderer, Scene, SceneOptions,
};
#[test]
fn hidden_ghosts_are_click_through_except_in_hide_tool() {
    let gpu = Gpu::headless().unwrap();
    let lib = ldraw::Library::open_default().unwrap();
    let doc = ldraw::Document::parse(
        b"1 4 0 0 0 1 0 0 0 1 0 0 0 1 3003.dat\n1 1 0 0 80 1 0 0 0 1 0 0 0 1 3003.dat\n",
        "hidden.ldr",
    );
    let scene = Scene::build(&lib, &doc.flatten(None), &SceneOptions::default());
    let mut renderer = Renderer::new(&gpu, COLOR_FORMAT, 1, &scene);
    let mut camera = Camera::framing(scene.bounds.0, scene.bounds.1, 1.);
    camera.yaw = 0.;
    camera.pitch = 0.;
    let mut picker = Picker::new(&gpu, (256, 256));
    assert_eq!(
        renderer.pick(&gpu, &mut picker, &camera, (256, 256), (128, 128)),
        Some(0)
    );
    for flags in [FLAG_HIDDEN, FLAG_HIDDEN_GHOST, FLAG_DELETE] {
        renderer.set_part_flags(&gpu, [(0, flags)]);
        assert_eq!(
            renderer.pick(&gpu, &mut picker, &camera, (256, 256), (128, 128)),
            Some(1)
        );
    }
    renderer.set_part_flags(&gpu, [(0, FLAG_HIDDEN_GHOST | FLAG_PICK_HIDDEN)]);
    assert_eq!(
        renderer.pick(&gpu, &mut picker, &camera, (256, 256), (128, 128)),
        Some(0)
    );
}
