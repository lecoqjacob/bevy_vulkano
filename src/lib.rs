#![allow(
    clippy::needless_question_mark,
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::module_inception
)]

/*
Pretty much the same as bevy_winit, but organized to use vulkano renderer backend.
This allows you to create your own pipelines for rendering.
 */
mod converters;
#[cfg(feature = "gui")]
mod gui;
mod utils;
mod vulkano_renderer;
mod winit_config;
mod winit_window_renderer;

use bevy::{
    app::{App, AppExit, CoreStage, EventReader, Events, ManualEventReader, Plugin},
    ecs::{system::IntoExclusiveSystem, world::World},
    input::{
        keyboard::KeyboardInput,
        mouse::{MouseButtonInput, MouseMotion, MouseScrollUnit, MouseWheel},
        touch::TouchInput,
    },
    math::{ivec2, DVec2, Vec2},
    prelude::ResMut,
    utils::tracing::{error, trace, warn},
    window::{
        CursorEntered, CursorLeft, CursorMoved, FileDragAndDrop, ReceivedCharacter,
        WindowBackendScaleFactorChanged, WindowCloseRequested, WindowCreated, WindowDescriptor,
        WindowFocused, WindowId, WindowMoved, WindowResized, WindowScaleFactorChanged, Windows,
    },
};
#[cfg(feature = "gui")]
use egui_winit_vulkano::Gui;
#[cfg(feature = "gui")]
pub use gui::*;
pub use utils::*;
use vulkano::{
    device::{DeviceExtensions, Features},
    instance::InstanceExtensions,
    swapchain::PresentMode,
};
pub use vulkano_renderer::*;
use winit::{
    dpi::{LogicalSize, PhysicalPosition},
    event::{self, DeviceEvent, Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop, EventLoopWindowTarget},
};
pub use winit_config::*;
pub use winit_window_renderer::*;

/// Vulkano related configurations
pub struct VulkanoWinitConfig {
    pub instance_extensions: InstanceExtensions,
    pub device_extensions: DeviceExtensions,
    pub features: Features,
    pub present_mode: PresentMode,
    pub layers: Vec<&'static str>,
}

impl Default for VulkanoWinitConfig {
    fn default() -> Self {
        VulkanoWinitConfig {
            instance_extensions: InstanceExtensions {
                ext_debug_utils: true,
                ..vulkano_win::required_extensions()
            },
            device_extensions: DeviceExtensions {
                khr_swapchain: true,
                ..DeviceExtensions::none()
            },
            features: Features::none(),
            present_mode: PresentMode::Fifo,
            layers: vec![],
        }
    }
}

/// Plugin that allows replacing Bevy's render backend with Vulkano. See examples for usage.
#[derive(Default)]
pub struct VulkanoWinitPlugin;

impl Plugin for VulkanoWinitPlugin {
    fn build(&self, app: &mut App) {
        // Create event loop, window and renderer (tied together...)
        let event_loop = EventLoop::new();

        // Insert config if none
        if app.world.get_resource::<VulkanoWinitConfig>().is_none() {
            app.insert_resource(VulkanoWinitConfig::default());
        }
        let config = app.world.get_resource::<VulkanoWinitConfig>().unwrap();
        // Primary Window
        let window_descriptor = app
            .world
            .get_resource::<WindowDescriptor>()
            .map(|descriptor| (*descriptor).clone())
            .unwrap_or_default();
        let window_id = WindowId::primary();
        let (renderer, window) = WinitWindows::create_window_with_renderer(
            &event_loop,
            window_id,
            &window_descriptor,
            config,
        );

        // Add window to bevy
        let mut windows = app.world.get_resource_mut::<Windows>().unwrap();
        windows.add(window);
        let mut window_created_events = app
            .world
            .get_resource_mut::<Events<WindowCreated>>()
            .unwrap();
        window_created_events.send(WindowCreated {
            id: window_id,
        });

        app.insert_non_send_resource(event_loop)
            .insert_resource(renderer)
            .insert_resource(BeforePipelineFuture(None))
            .insert_resource(AfterPipelineFuture(None))
            .set_runner(winit_runner)
            .add_system_to_stage(CoreStage::PreUpdate, resize_renderer)
            .add_system_to_stage(CoreStage::PostUpdate, change_window.exclusive_system());

        #[cfg(feature = "gui")]
        app.add_plugin(GuiPlgin::default());
    }
}

fn resize_renderer(
    mut renderer: ResMut<Renderer>,
    mut resize_event_reader: EventReader<WindowResized>,
) {
    if let Some(_e) = resize_event_reader.iter().last() {
        // Recreates swapchain...
        renderer.resize();
    }
}

fn change_window(world: &mut World) {
    let world = world.cell();
    let renderer = world.get_resource::<Renderer>().unwrap();
    let mut windows = world.get_resource_mut::<Windows>().unwrap();

    for bevy_window in windows.iter_mut() {
        let id = bevy_window.id();
        for command in bevy_window.drain_commands() {
            match command {
                bevy::window::WindowCommand::SetWindowMode {
                    mode,
                    resolution: (width, height),
                } => {
                    let window = renderer.window();
                    match mode {
                        bevy::window::WindowMode::BorderlessFullscreen => {
                            window.set_fullscreen(Some(winit::window::Fullscreen::Borderless(None)))
                        }
                        bevy::window::WindowMode::Fullscreen => {
                            window.set_fullscreen(Some(winit::window::Fullscreen::Exclusive(
                                get_best_videomode(&window.current_monitor().unwrap()),
                            )))
                        }
                        bevy::window::WindowMode::SizedFullscreen => window.set_fullscreen(Some(
                            winit::window::Fullscreen::Exclusive(get_fitting_videomode(
                                &window.current_monitor().unwrap(),
                                width,
                                height,
                            )),
                        )),
                        bevy::window::WindowMode::Windowed => window.set_fullscreen(None),
                    }
                }
                bevy::window::WindowCommand::SetTitle {
                    title,
                } => {
                    let window = renderer.window();
                    window.set_title(&title);
                }
                bevy::window::WindowCommand::SetScaleFactor {
                    scale_factor,
                } => {
                    let mut window_dpi_changed_events = world
                        .get_resource_mut::<Events<WindowScaleFactorChanged>>()
                        .unwrap();
                    window_dpi_changed_events.send(WindowScaleFactorChanged {
                        id,
                        scale_factor,
                    });
                }
                bevy::window::WindowCommand::SetResolution {
                    logical_resolution: (width, height),
                    scale_factor,
                } => {
                    let window = renderer.window();
                    window.set_inner_size(
                        winit::dpi::LogicalSize::new(width, height)
                            .to_physical::<f64>(scale_factor),
                    );
                }
                bevy::window::WindowCommand::SetVsync {
                    ..
                } => (),
                bevy::window::WindowCommand::SetResizable {
                    resizable,
                } => {
                    let window = renderer.window();
                    window.set_resizable(resizable);
                }
                bevy::window::WindowCommand::SetDecorations {
                    decorations,
                } => {
                    let window = renderer.window();
                    window.set_decorations(decorations);
                }
                bevy::window::WindowCommand::SetCursorIcon {
                    icon,
                } => {
                    let window = renderer.window();
                    window.set_cursor_icon(converters::convert_cursor_icon(icon));
                }
                bevy::window::WindowCommand::SetCursorLockMode {
                    locked,
                } => {
                    let window = renderer.window();
                    window
                        .set_cursor_grab(locked)
                        .unwrap_or_else(|e| error!("Unable to un/grab cursor: {}", e));
                }
                bevy::window::WindowCommand::SetCursorVisibility {
                    visible,
                } => {
                    let window = renderer.window();
                    window.set_cursor_visible(visible);
                }
                bevy::window::WindowCommand::SetCursorPosition {
                    position,
                } => {
                    let window = renderer.window();
                    let inner_size = window.inner_size().to_logical::<f32>(window.scale_factor());
                    window
                        .set_cursor_position(winit::dpi::LogicalPosition::new(
                            position.x,
                            inner_size.height - position.y,
                        ))
                        .unwrap_or_else(|e| error!("Unable to set cursor position: {}", e));
                }
                bevy::window::WindowCommand::SetMaximized {
                    maximized,
                } => {
                    let window = renderer.window();
                    window.set_maximized(maximized)
                }
                bevy::window::WindowCommand::SetMinimized {
                    minimized,
                } => {
                    let window = renderer.window();
                    window.set_minimized(minimized)
                }
                bevy::window::WindowCommand::SetPosition {
                    position,
                } => {
                    let window = renderer.window();
                    window.set_outer_position(PhysicalPosition {
                        x: position[0],
                        y: position[1],
                    });
                }
                bevy::window::WindowCommand::SetResizeConstraints {
                    resize_constraints,
                } => {
                    let window = renderer.window();
                    let constraints = resize_constraints.check_constraints();
                    let min_inner_size = LogicalSize {
                        width: constraints.min_width,
                        height: constraints.min_height,
                    };
                    let max_inner_size = LogicalSize {
                        width: constraints.max_width,
                        height: constraints.max_height,
                    };

                    window.set_min_inner_size(Some(min_inner_size));
                    if constraints.max_width.is_finite() && constraints.max_height.is_finite() {
                        window.set_max_inner_size(Some(max_inner_size));
                    }
                }
            }
        }
    }
}

fn run<F>(event_loop: EventLoop<()>, event_handler: F) -> !
where
    F: 'static + FnMut(Event<'_, ()>, &EventLoopWindowTarget<()>, &mut ControlFlow),
{
    event_loop.run(event_handler)
}

#[cfg(any(
    target_os = "windows",
    target_os = "macos",
    target_os = "linux",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
))]
fn run_return<F>(event_loop: &mut EventLoop<()>, event_handler: F)
where
    F: FnMut(Event<'_, ()>, &EventLoopWindowTarget<()>, &mut ControlFlow),
{
    use winit::platform::run_return::EventLoopExtRunReturn;
    event_loop.run_return(event_handler)
}

#[cfg(not(any(
    target_os = "windows",
    target_os = "macos",
    target_os = "linux",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
)))]
fn run_return<F>(_event_loop: &mut EventLoop<()>, _event_handler: F)
where
    F: FnMut(Event<'_, ()>, &EventLoopWindowTarget<()>, &mut ControlFlow),
{
    panic!("Run return is not supported on this platform!")
}

pub fn winit_runner(app: App) {
    winit_runner_with(app);
}

pub fn winit_runner_with(mut app: App) {
    let mut event_loop = app.world.remove_non_send::<EventLoop<()>>().unwrap();
    let mut app_exit_event_reader = ManualEventReader::<AppExit>::default();
    app.world.insert_non_send(event_loop.create_proxy());

    trace!("Entering winit event loop");

    let should_return_from_run = app
        .world
        .get_resource::<WinitConfig>()
        .map_or(false, |config| config.return_from_run);

    let mut active = true;

    let event_handler = move |event: Event<()>,
                              _event_loop: &EventLoopWindowTarget<()>,
                              control_flow: &mut ControlFlow| {
        *control_flow = ControlFlow::Poll;

        if let Some(app_exit_events) = app.world.get_resource_mut::<Events<AppExit>>() {
            if app_exit_event_reader
                .iter(&app_exit_events)
                .next_back()
                .is_some()
            {
                *control_flow = ControlFlow::Exit;
            }
        }

        #[cfg(feature = "gui")]
        app.world
            .get_non_send_resource_mut::<Gui>()
            .unwrap()
            .update(&event);

        match event {
            event::Event::WindowEvent {
                event,
                window_id: winit_window_id,
                ..
            } => {
                let world = app.world.cell();
                let renderer = world.get_resource::<Renderer>().unwrap();
                let mut windows = world.get_resource_mut::<Windows>().unwrap();
                let window_id = if winit_window_id == renderer.window().id() {
                    WindowId::primary()
                } else {
                    warn!(
                        "Skipped event for unknown winit Window Id {:?}",
                        winit_window_id
                    );
                    return;
                };

                let window = if let Some(window) = windows.get_mut(window_id) {
                    window
                } else {
                    warn!("Skipped event for unknown Window Id {:?}", winit_window_id);
                    return;
                };

                match event {
                    WindowEvent::Resized(size) => {
                        window.update_actual_size_from_backend(size.width, size.height);
                        let mut resize_events =
                            world.get_resource_mut::<Events<WindowResized>>().unwrap();
                        resize_events.send(WindowResized {
                            id: window_id,
                            width: window.width(),
                            height: window.height(),
                        });
                    }
                    WindowEvent::CloseRequested => {
                        let mut window_close_requested_events = world
                            .get_resource_mut::<Events<WindowCloseRequested>>()
                            .unwrap();
                        window_close_requested_events.send(WindowCloseRequested {
                            id: window_id,
                        });
                    }
                    WindowEvent::KeyboardInput {
                        ref input, ..
                    } => {
                        let mut keyboard_input_events =
                            world.get_resource_mut::<Events<KeyboardInput>>().unwrap();
                        keyboard_input_events.send(converters::convert_keyboard_input(input));
                    }
                    WindowEvent::CursorMoved {
                        position, ..
                    } => {
                        let mut cursor_moved_events =
                            world.get_resource_mut::<Events<CursorMoved>>().unwrap();
                        let winit_window = renderer.window();
                        let inner_size = winit_window.inner_size();

                        // move origin to bottom left
                        let y_position = inner_size.height as f64 - position.y;

                        let physical_position = DVec2::new(position.x, y_position);
                        window
                            .update_cursor_physical_position_from_backend(Some(physical_position));

                        cursor_moved_events.send(CursorMoved {
                            id: window_id,
                            position: (physical_position / window.scale_factor()).as_vec2(),
                        });
                    }
                    WindowEvent::CursorEntered {
                        ..
                    } => {
                        let mut cursor_entered_events =
                            world.get_resource_mut::<Events<CursorEntered>>().unwrap();
                        cursor_entered_events.send(CursorEntered {
                            id: window_id,
                        });
                    }
                    WindowEvent::CursorLeft {
                        ..
                    } => {
                        let mut cursor_left_events =
                            world.get_resource_mut::<Events<CursorLeft>>().unwrap();
                        window.update_cursor_physical_position_from_backend(None);
                        cursor_left_events.send(CursorLeft {
                            id: window_id,
                        });
                    }
                    WindowEvent::MouseInput {
                        state,
                        button,
                        ..
                    } => {
                        let mut mouse_button_input_events = world
                            .get_resource_mut::<Events<MouseButtonInput>>()
                            .unwrap();
                        mouse_button_input_events.send(MouseButtonInput {
                            button: converters::convert_mouse_button(button),
                            state: converters::convert_element_state(state),
                        });
                    }
                    WindowEvent::MouseWheel {
                        delta, ..
                    } => match delta {
                        event::MouseScrollDelta::LineDelta(x, y) => {
                            let mut mouse_wheel_input_events =
                                world.get_resource_mut::<Events<MouseWheel>>().unwrap();
                            mouse_wheel_input_events.send(MouseWheel {
                                unit: MouseScrollUnit::Line,
                                x,
                                y,
                            });
                        }
                        event::MouseScrollDelta::PixelDelta(p) => {
                            let mut mouse_wheel_input_events =
                                world.get_resource_mut::<Events<MouseWheel>>().unwrap();
                            mouse_wheel_input_events.send(MouseWheel {
                                unit: MouseScrollUnit::Pixel,
                                x: p.x as f32,
                                y: p.y as f32,
                            });
                        }
                    },
                    WindowEvent::Touch(touch) => {
                        let mut touch_input_events =
                            world.get_resource_mut::<Events<TouchInput>>().unwrap();

                        let mut location = touch.location.to_logical(window.scale_factor());

                        // On a mobile window, the start is from the top while on PC/Linux/OSX from
                        // bottom
                        if cfg!(target_os = "android") || cfg!(target_os = "ios") {
                            let window_height = windows.get_primary().unwrap().height();
                            location.y = window_height - location.y;
                        }
                        touch_input_events.send(converters::convert_touch_input(touch, location));
                    }
                    WindowEvent::ReceivedCharacter(c) => {
                        let mut char_input_events = world
                            .get_resource_mut::<Events<ReceivedCharacter>>()
                            .unwrap();

                        char_input_events.send(ReceivedCharacter {
                            id: window_id,
                            char: c,
                        })
                    }
                    WindowEvent::ScaleFactorChanged {
                        scale_factor,
                        new_inner_size,
                    } => {
                        let mut backend_scale_factor_change_events = world
                            .get_resource_mut::<Events<WindowBackendScaleFactorChanged>>()
                            .unwrap();
                        backend_scale_factor_change_events.send(WindowBackendScaleFactorChanged {
                            id: window_id,
                            scale_factor,
                        });
                        let prior_factor = window.scale_factor();
                        window.update_scale_factor_from_backend(scale_factor);
                        let new_factor = window.scale_factor();
                        if let Some(forced_factor) = window.scale_factor_override() {
                            // If there is a scale factor override, then force that to be used
                            // Otherwise, use the OS suggested size
                            // We have already told the OS about our resize constraints, so
                            // the new_inner_size should take those into account
                            *new_inner_size = winit::dpi::LogicalSize::new(
                                window.requested_width(),
                                window.requested_height(),
                            )
                            .to_physical::<u32>(forced_factor);
                        } else if approx::relative_ne!(new_factor, prior_factor) {
                            let mut scale_factor_change_events = world
                                .get_resource_mut::<Events<WindowScaleFactorChanged>>()
                                .unwrap();

                            scale_factor_change_events.send(WindowScaleFactorChanged {
                                id: window_id,
                                scale_factor,
                            });
                        }

                        let new_logical_width = new_inner_size.width as f64 / new_factor;
                        let new_logical_height = new_inner_size.height as f64 / new_factor;
                        if approx::relative_ne!(window.width() as f64, new_logical_width)
                            || approx::relative_ne!(window.height() as f64, new_logical_height)
                        {
                            let mut resize_events =
                                world.get_resource_mut::<Events<WindowResized>>().unwrap();
                            resize_events.send(WindowResized {
                                id: window_id,
                                width: new_logical_width as f32,
                                height: new_logical_height as f32,
                            });
                        }
                        window.update_actual_size_from_backend(
                            new_inner_size.width,
                            new_inner_size.height,
                        );
                    }
                    WindowEvent::Focused(focused) => {
                        window.update_focused_status_from_backend(focused);
                        let mut focused_events =
                            world.get_resource_mut::<Events<WindowFocused>>().unwrap();
                        focused_events.send(WindowFocused {
                            id: window_id,
                            focused,
                        });
                    }
                    WindowEvent::DroppedFile(path_buf) => {
                        let mut events =
                            world.get_resource_mut::<Events<FileDragAndDrop>>().unwrap();
                        events.send(FileDragAndDrop::DroppedFile {
                            id: window_id,
                            path_buf,
                        });
                    }
                    WindowEvent::HoveredFile(path_buf) => {
                        let mut events =
                            world.get_resource_mut::<Events<FileDragAndDrop>>().unwrap();
                        events.send(FileDragAndDrop::HoveredFile {
                            id: window_id,
                            path_buf,
                        });
                    }
                    WindowEvent::HoveredFileCancelled => {
                        let mut events =
                            world.get_resource_mut::<Events<FileDragAndDrop>>().unwrap();
                        events.send(FileDragAndDrop::HoveredFileCancelled {
                            id: window_id,
                        });
                    }
                    WindowEvent::Moved(position) => {
                        let position = ivec2(position.x, position.y);
                        window.update_actual_position_from_backend(position);
                        let mut events = world.get_resource_mut::<Events<WindowMoved>>().unwrap();
                        events.send(WindowMoved {
                            id: window_id,
                            position,
                        });
                    }
                    _ => {}
                }
            }
            event::Event::DeviceEvent {
                event: DeviceEvent::MouseMotion {
                    delta,
                },
                ..
            } => {
                let mut mouse_motion_events =
                    app.world.get_resource_mut::<Events<MouseMotion>>().unwrap();
                mouse_motion_events.send(MouseMotion {
                    delta: Vec2::new(delta.0 as f32, delta.1 as f32),
                });
            }
            event::Event::Suspended => {
                active = false;
            }
            event::Event::Resumed => {
                active = true;
            }
            event::Event::MainEventsCleared => {
                if active {
                    app.update();
                }
            }
            _ => (),
        }
    };
    if should_return_from_run {
        run_return(&mut event_loop, event_handler);
    } else {
        run(event_loop, event_handler);
    }
}