use ldraw::{Document, Library};
use render::{COLOR_FORMAT, Camera, Gpu, Offscreen, Picker, Renderer, Scene, SceneOptions};

#[test]
fn guide_renders_but_is_never_pickable() {
    let gpu = Gpu::headless().expect("GPU for rendering integration test");
    let lib = Library::open_default().unwrap();
    let scene = Scene::build(&lib, &[], &SceneOptions::default());
    let mut renderer = Renderer::new(&gpu, COLOR_FORMAT, 1, &scene);
    let mut cam = Camera::framing(
        glam::vec3(-160.0, 0.0, -160.0),
        glam::vec3(160.0, 0.0, 160.0),
        1.0,
    );
    let off = Offscreen::new(&gpu, 1, 256, 256);
    off.render(&gpu, &renderer, &cam);
    let without = off.read_rgba(&gpu);
    renderer.enable_ground(&gpu, COLOR_FORMAT, scene.bounds);
    cam.radius = cam.radius.max(renderer.ground_radius(cam.target));
    off.render(&gpu, &renderer, &cam);
    let with = off.read_rgba(&gpu);
    let changed = without
        .chunks_exact(4)
        .zip(with.chunks_exact(4))
        .filter(|(a, b)| a != b)
        .count();
    assert!(
        changed > 1000,
        "baseplate must visibly render: {changed} pixels changed"
    );
    let mut picker = Picker::new(&gpu, (256, 256));
    assert_eq!(
        renderer.pick(&gpu, &mut picker, &cam, (256, 256), (128, 128)),
        None
    );

    let doc = Document::parse(
        b"0 Pick test\n1 4 0 0 0 1 0 0 0 1 0 0 0 1 3003.dat\n",
        "pick.ldr",
    );
    let scene = Scene::build(&lib, &doc.flatten(None), &SceneOptions::default());
    let mut renderer = Renderer::new(&gpu, COLOR_FORMAT, 1, &scene);
    renderer.enable_ground(&gpu, COLOR_FORMAT, scene.bounds);
    let mut cam = Camera::framing(scene.bounds.0, scene.bounds.1, 1.0);
    cam.radius = cam.radius.max(renderer.ground_radius(cam.target));
    assert_eq!(
        renderer.pick(&gpu, &mut picker, &cam, (256, 256), (128, 128)),
        Some(0)
    );
}
