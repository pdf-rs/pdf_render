use crate::*;

pub fn main() -> Result<()> {
    // TODO: initializing both env_logger and console_logger fails on wasm.
    // Figure out a more principled approach.
    #[cfg(not(target_arch = "wasm32"))]
    env_logger::init();
    let args = Args::parse();
    let scenes = args.args.select_scene_set(Args::command)?;
    if let Some(scenes) = scenes {
        let event_loop = EventLoopBuilder::<()>::with_user_event().build();

        let render_cx = RenderContext::new().unwrap();
        
        run(event_loop, args, scenes, render_cx);
    }
    Ok(())
}
