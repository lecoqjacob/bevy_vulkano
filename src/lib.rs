#![allow(
    clippy::needless_question_mark,
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::module_inception,
    clippy::single_match,
    clippy::match_like_matches_macro
)]

/*
Pretty much the same as bevy_winit, but organized to use vulkano renderer backend.
This allows you to create your own pipelines for rendering.
 */
mod converters;
mod pipeline_sync_data;
mod vulkano_windows;

use bevy::{
    app::{App, AppExit, Plugin},
    ecs::{
        event::{Events, ManualEventReader},
        system::SystemState,
    },
    input::{
        keyboard::KeyboardInput,
        mouse::{MouseButtonInput, MouseMotion, MouseScrollUnit, MouseWheel},
        touch::TouchInput,
    },
    math::{ivec2, Vec2},
    prelude::*,
    utils::HashSet,
    window::{
        CursorEntered, CursorLeft, CursorMoved, ExitCondition, FileDragAndDrop, PrimaryWindow,
        ReceivedCharacter, WindowBackendScaleFactorChanged, WindowCloseRequested, WindowClosed,
        WindowCreated, WindowFocused, WindowMoved, WindowResized, WindowScaleFactorChanged,
    },
};
#[cfg(feature = "gui")]
pub use egui_winit_vulkano;
pub use pipeline_sync_data::*;
use vulkano_util::context::{VulkanoConfig, VulkanoContext};
pub use vulkano_windows::*;
use winit::{
    event::{self, DeviceEvent, Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop, EventLoopWindowTarget},
    window::WindowId,
};

/// Vulkano & winit related configurations
pub struct VulkanoWinitConfig {
    /// Configures the winit library to return control to the main thread after
    /// the [run](bevy_app::App::run) loop is exited. Winit strongly recommends
    /// avoiding this when possible. Before using this please read and understand
    /// the [caveats](winit::platform::run_return::EventLoopExtRunReturn::run_return)
    /// in the winit documentation.
    ///
    /// This feature is only available on desktop `target_os` configurations.
    /// Namely `windows`, `macos`, `linux`, `dragonfly`, `freebsd`, `netbsd`, and
    /// `openbsd`. If set to true on an unsupported platform
    /// [run](bevy_app::App::run) will panic.
    pub return_from_run: bool,
    /// Vulkano backend related configs
    pub vulkano_config: VulkanoConfig,
    /// Whether the image gets cleared each frame by gui integration. This is only relevant if
    /// `gui` feature is set.
    /// Default is true, thus you need to clear the image you intend to draw gui on
    #[cfg(feature = "gui")]
    pub is_gui_overlay: bool,
    /// Control whether you want to run the app with or without a window
    pub add_primary_window: bool, // TODO: is this needed?
}

impl Default for VulkanoWinitConfig {
    fn default() -> Self {
        VulkanoWinitConfig {
            return_from_run: false,
            vulkano_config: VulkanoConfig::default(),
            #[cfg(feature = "gui")]
            is_gui_overlay: true,
            add_primary_window: true,
        }
    }
}

/// Wrapper around [`VulkanoContext`] to allow using them as resources
#[derive(Resource)]
pub struct BevyVulkanoContext {
    pub context: VulkanoContext,
}

/// Plugin that allows replacing Bevy's render backend with Vulkano. See examples for usage.
#[derive(Default)]
pub struct VulkanoWinitPlugin {
    pub window_descriptor: Window,
}

impl Plugin for VulkanoWinitPlugin {
    fn build(&self, app: &mut App) {
        // Create event loop, window and renderer (tied together...)
        let event_loop = EventLoop::new();

        // Retrieve config, or use default.
        let config = if app
            .world
            .get_non_send_resource::<VulkanoWinitConfig>()
            .is_none()
        {
            VulkanoWinitConfig::default()
        } else {
            app.world
                .remove_non_send_resource::<VulkanoWinitConfig>()
                .unwrap()
        };

        // Create vulkano context using the vulkano config from config
        let VulkanoWinitConfig {
            vulkano_config, ..
        } = config;
        let vulkano_context = VulkanoContext::new(vulkano_config);
        // Place config back as resource. Vulkano config will be useless at this point.
        let new_config = VulkanoWinitConfig {
            vulkano_config: VulkanoConfig::default(),
            ..config
        };
        app.insert_non_send_resource(new_config);

        let window_plugin = bevy::window::WindowPlugin {
            // This lib controls exiting all on close. (true)
            exit_condition: ExitCondition::DontExit,
            primary_window: Some(self.window_descriptor.clone()),
            ..default()
        };

        // Insert window plugin, vulkano context, windows resource & pipeline data
        app.add_plugin(window_plugin)
            .init_non_send_resource::<BevyVulkanoWindows>()
            .init_resource::<PipelineSyncData>()
            .insert_resource(BevyVulkanoContext {
                context: vulkano_context,
            });

        // Create initial window
        handle_initial_window_events(&mut app.world, &event_loop);

        app.insert_non_send_resource(event_loop)
            .set_runner(winit_runner)
            .add_systems(
                (update_on_resize_system, exit_on_window_close_system)
                    .in_base_set(CoreSet::PreUpdate),
            )
            .add_system(change_window.in_base_set(CoreSet::PostUpdate));

        // Add gui begin frame system
        #[cfg(feature = "gui")]
        {
            app.add_system_to_stage(CoreStage::PreUpdate, begin_egui_frame_system);
        }
    }
}

fn update_on_resize_system(
    mut pipeline_data: ResMut<PipelineSyncData>,
    mut windows: NonSendMut<BevyVulkanoWindows>,
    mut window_resized_events: EventReader<WindowResized>,
    mut window_created_events: EventReader<WindowCreated>,
) {
    let mut changed_window_ids = HashSet::new();
    changed_window_ids.extend(window_created_events.iter().map(|event| event.window));
    changed_window_ids.extend(window_resized_events.iter().map(|event| event.window));

    for id in changed_window_ids {
        #[cfg(not(feature = "gui"))]
        if let Some(window_renderer) = windows.get_window_renderer_mut(id) {
            // Swap chain will be resized at the beginning of next frame. But user should update pipeline frame data
            window_renderer.resize();
            // Insert or update pipeline frame data
            pipeline_data.add(SyncData {
                window_entity: id,
                before: None,
                after: None,
            });
        }
        #[cfg(feature = "gui")]
        if let Some((window_renderer, _)) = windows.get_window_renderer_mut(id) {
            // Swap chain will be resized at the beginning of next frame. But user should update pipeline frame data
            window_renderer.resize();
            // Insert or update pipeline frame data
            pipeline_data.add(SyncData {
                window_id: id,
                before: None,
                after: None,
            });
        }
    }
}

fn change_window(world: &mut World) {
    let mut state: SystemState<(
        NonSendMut<BevyVulkanoWindows>,
        ResMut<PipelineSyncData>,
        Query<(Entity, &Window)>,
        Query<Entity, With<PrimaryWindow>>,
        EventWriter<AppExit>,
        EventWriter<WindowClosed>,
    )> = SystemState::from_world(world);

    let (
        mut vulkano_winit_windows,
        mut pipeline_sync_data,
        mut windows,
        primary_window_entity,
        mut app_exit_events,
        mut window_closed_events,
    ) = state.get_mut(world);

    let mut removed_windows = vec![];

    // TODO: This is a big one. Bevy doesnt send commands anymore. They are directly linked to winit i beleive

    for (window, bevy_window) in windows.iter_mut() {
        // for command in bevy_window.drain_commands() {
        //     match command {
        //         bevy::window::WindowCommand::SetWindowMode {
        //             mode,
        //             resolution,
        //         } => {
        //             let window = vulkano_winit_windows.get_winit_window(id).unwrap();
        //             match mode {
        //                 bevy::window::WindowMode::BorderlessFullscreen => {
        //                     window.set_fullscreen(Some(winit::window::Fullscreen::Borderless(None)))
        //                 }
        //                 bevy::window::WindowMode::Fullscreen => {
        //                     window.set_fullscreen(Some(winit::window::Fullscreen::Exclusive(
        //                         get_best_videomode(&window.current_monitor().unwrap()),
        //                     )))
        //                 }
        //                 bevy::window::WindowMode::SizedFullscreen => window.set_fullscreen(Some(
        //                     winit::window::Fullscreen::Exclusive(get_fitting_videomode(
        //                         &window.current_monitor().unwrap(),
        //                         resolution.x,
        //                         resolution.y,
        //                     )),
        //                 )),
        //                 bevy::window::WindowMode::Windowed => window.set_fullscreen(None),
        //             }
        //         }
        //         bevy::window::WindowCommand::SetTitle {
        //             title,
        //         } => {
        //             let window = vulkano_winit_windows.get_winit_window(id).unwrap();
        //             window.set_title(&title);
        //         }
        //         bevy::window::WindowCommand::SetScaleFactor {
        //             scale_factor,
        //         } => {
        //             let mut window_dpi_changed_events = world
        //                 .get_resource_mut::<Events<WindowScaleFactorChanged>>()
        //                 .unwrap();
        //             window_dpi_changed_events.send(WindowScaleFactorChanged {
        //                 window,
        //                 scale_factor,
        //             });
        //         }
        //         bevy::window::WindowCommand::SetResolution {
        //             logical_resolution,
        //             scale_factor,
        //         } => {
        //             let window = vulkano_winit_windows.get_winit_window(id).unwrap();
        //             window.set_inner_size(
        //                 winit::dpi::LogicalSize::new(logical_resolution.x, logical_resolution.y)
        //                     .to_physical::<f64>(scale_factor),
        //             );
        //         }
        //         bevy::window::WindowCommand::SetPresentMode {
        //             present_mode,
        //         } => {
        //             let present_mode = match present_mode {
        //                 bevy::window::PresentMode::AutoVsync => {
        //                     vulkano::swapchain::PresentMode::FifoRelaxed
        //                 }
        //                 bevy::window::PresentMode::AutoNoVsync => {
        //                     vulkano::swapchain::PresentMode::Immediate
        //                 }
        //                 bevy::window::PresentMode::Fifo => vulkano::swapchain::PresentMode::Fifo,
        //                 bevy::window::PresentMode::Immediate => {
        //                     vulkano::swapchain::PresentMode::Immediate
        //                 }
        //                 bevy::window::PresentMode::Mailbox => {
        //                     vulkano::swapchain::PresentMode::Mailbox
        //                 }
        //             };
        //             let wr = {
        //                 #[cfg(not(feature = "gui"))]
        //                 let wr = vulkano_winit_windows.get_window_renderer_mut(id).unwrap();
        //                 #[cfg(feature = "gui")]
        //                 let (wr, _) = vulkano_winit_windows.get_window_renderer_mut(id).unwrap();
        //                 wr
        //             };
        //             wr.set_present_mode(present_mode);
        //         }
        //         bevy::window::WindowCommand::SetResizable {
        //             resizable,
        //         } => {
        //             let window = vulkano_winit_windows.get_winit_window(id).unwrap();
        //             window.set_resizable(resizable);
        //         }
        //         bevy::window::WindowCommand::SetDecorations {
        //             decorations,
        //         } => {
        //             let window = vulkano_winit_windows.get_winit_window(id).unwrap();
        //             window.set_decorations(decorations);
        //         }
        //         bevy::window::WindowCommand::SetCursorIcon {
        //             icon,
        //         } => {
        //             let window = vulkano_winit_windows.get_winit_window(id).unwrap();
        //             window.set_cursor_icon(converters::convert_cursor_icon(icon));
        //         }
        //         bevy::window::WindowCommand::SetCursorGrabMode {
        //             grab_mode,
        //         } => {
        //             let window = vulkano_winit_windows.get_winit_window(id).unwrap();
        //             window
        //                 .set_cursor_grab(match grab_mode {
        //                     bevy::window::CursorGrabMode::Confined => CursorGrabMode::Confined,
        //                     bevy::window::CursorGrabMode::Locked => CursorGrabMode::Locked,
        //                     bevy::window::CursorGrabMode::None => CursorGrabMode::None,
        //                 })
        //                 .unwrap_or_else(|e| error!("Unable to un/grab cursor: {}", e));
        //         }
        //         bevy::window::WindowCommand::SetCursorVisibility {
        //             visible,
        //         } => {
        //             let window = vulkano_winit_windows.get_winit_window(id).unwrap();
        //             window.set_cursor_visible(visible);
        //         }
        //         bevy::window::WindowCommand::SetCursorPosition {
        //             position,
        //         } => {
        //             let window = vulkano_winit_windows.get_winit_window(id).unwrap();
        //             let inner_size = window.inner_size().to_logical::<f32>(window.scale_factor());
        //             window
        //                 .set_cursor_position(winit::dpi::LogicalPosition::new(
        //                     position.x,
        //                     inner_size.height - position.y,
        //                 ))
        //                 .unwrap_or_else(|e| error!("Unable to set cursor position: {}", e));
        //         }
        //         bevy::window::WindowCommand::SetMaximized {
        //             maximized,
        //         } => {
        //             let window = vulkano_winit_windows.get_winit_window(id).unwrap();
        //             window.set_maximized(maximized)
        //         }
        //         bevy::window::WindowCommand::SetMinimized {
        //             minimized,
        //         } => {
        //             let window = vulkano_winit_windows.get_winit_window(id).unwrap();
        //             window.set_minimized(minimized)
        //         }
        //         bevy::window::WindowCommand::SetPosition {
        //             monitor_selection: _,
        //             position,
        //         } => {
        //             let window = vulkano_winit_windows.get_winit_window(id).unwrap();
        //             window.set_outer_position(PhysicalPosition {
        //                 x: position[0],
        //                 y: position[1],
        //             });
        //         }
        //         bevy::window::WindowCommand::Center(monitor_selection) => {
        //             let window = vulkano_winit_windows.get_winit_window(id).unwrap();

        //             let maybe_monitor = match monitor_selection {
        //                 bevy::window::MonitorSelection::Current => window.current_monitor(),
        //                 bevy::window::MonitorSelection::Primary => window.primary_monitor(),
        //                 bevy::window::MonitorSelection::Index(n) => {
        //                     window.available_monitors().nth(n)
        //                 }
        //             };

        //             if let Some(monitor) = maybe_monitor {
        //                 let screen_size = monitor.size();

        //                 let window_size = window.outer_size();

        //                 window.set_outer_position(PhysicalPosition {
        //                     x: screen_size.width.saturating_sub(window_size.width) as f64 / 2.
        //                         + monitor.position().x as f64,
        //                     y: screen_size.height.saturating_sub(window_size.height) as f64 / 2.
        //                         + monitor.position().y as f64,
        //                 });
        //             } else {
        //                 warn!("Couldn't get monitor selected with: {monitor_selection:?}");
        //             }
        //         }
        //         bevy::window::WindowCommand::SetResizeConstraints {
        //             resize_constraints,
        //         } => {
        //             let window = vulkano_winit_windows.get_winit_window(id).unwrap();
        //             let constraints = resize_constraints.check_constraints();
        //             let min_inner_size = LogicalSize {
        //                 width: constraints.min_width,
        //                 height: constraints.min_height,
        //             };
        //             let max_inner_size = LogicalSize {
        //                 width: constraints.max_width,
        //                 height: constraints.max_height,
        //             };

        //             window.set_min_inner_size(Some(min_inner_size));
        //             if constraints.max_width.is_finite() && constraints.max_height.is_finite() {
        //                 window.set_max_inner_size(Some(max_inner_size));
        //             }
        //         }
        //         bevy::window::WindowCommand::Close => {
        //             // Since we have borrowed `windows` to iterate through them, we can't remove the window from it.
        //             // Add the removal requests to a queue to solve this
        //             removed_windows.push(id);
        //             // No need to run any further commands - this drops the rest of the commands, although the `bevy_window::Window` will be dropped later anyway
        //             break;
        //         }
        //     }
        // }
    }

    if !removed_windows.is_empty() {
        for window in removed_windows {
            let (app_close, window_close) = close_window(
                window,
                &mut vulkano_winit_windows,
                primary_window_entity.get_single(),
                &mut pipeline_sync_data,
            );

            if app_close {
                app_exit_events.send(AppExit);
            } else if window_close {
                window_closed_events.send(WindowClosed {
                    window,
                })
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
fn run_return<F>(event_loop: &mut EventLoop<()>, event_handler: F) -> i32
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
    let mut event_loop = app
        .world
        .remove_non_send_resource::<EventLoop<()>>()
        .unwrap();
    let mut app_exit_event_reader = ManualEventReader::<AppExit>::default();
    app.world
        .insert_non_send_resource(event_loop.create_proxy());

    trace!("Entering winit event loop");

    let should_return_from_run = app
        .world
        .get_non_send_resource::<VulkanoWinitConfig>()
        .map_or(false, |config| config.return_from_run);

    let mut active = true;

    let event_handler = move |event: Event<()>,
                              event_loop: &EventLoopWindowTarget<()>,
                              control_flow: &mut ControlFlow| {
        *control_flow = ControlFlow::Poll;

        if let Some(app_exit_events) = app.world.get_resource_mut::<Events<AppExit>>() {
            if app_exit_event_reader
                .iter(&app_exit_events)
                .next()
                .is_some()
            {
                *control_flow = ControlFlow::Exit;
            }
        }

        #[cfg(feature = "gui")]
        let mut skip_window_event = false;
        #[cfg(not(feature = "gui"))]
        let skip_window_event = false;
        // Update gui with winit event
        #[cfg(feature = "gui")]
        {
            match &event {
                event::Event::WindowEvent {
                    event: window_event,
                    window_id: winit_window_id,
                    ..
                } => {
                    let world = app.world.cell();
                    let mut vulkano_winit_windows = world
                        .get_non_send_resource_mut::<BevyVulkanoWindows>()
                        .unwrap();
                    let window_id = if let Some(window_id) =
                        vulkano_winit_windows.get_window_id(*winit_window_id)
                    {
                        window_id
                    } else {
                        return;
                    };
                    if let Some((_, gui)) = vulkano_winit_windows.get_window_renderer_mut(window_id)
                    {
                        // Update egui with the window event. If false, we should skip the event in bevy
                        skip_window_event = gui.update(window_event);
                    }
                }
                _ => (),
            }
        }

        // Handle touch separately here, and in case of gui, we don't want to skip the touch event
        match &event {
            event::Event::WindowEvent {
                event,
                window_id: winit_window_id,
                ..
            } => {
                let mut state: SystemState<(
                    NonSend<BevyVulkanoWindows>,
                    Query<&mut Window>,
                    ResMut<Events<TouchInput>>,
                )> = SystemState::from_world(&mut app.world);

                let (vulkano_winit_windows, mut windows, mut touch_input_events) =
                    state.get_mut(&mut app.world);

                let window_entity = if let Some(window_id) =
                    vulkano_winit_windows.get_window_entity(*winit_window_id)
                {
                    window_id
                } else {
                    warn!(
                        "Skipped event for unknown winit Window Id {:?}",
                        winit_window_id
                    );
                    return;
                };

                let window = if let Ok(window) = windows.get_mut(window_entity) {
                    window
                } else {
                    warn!("Skipped event for unknown Window Id {:?}", winit_window_id);
                    return;
                };

                match event {
                    WindowEvent::Touch(touch) => {
                        let mut location = touch.location.to_logical(window.scale_factor());

                        // On a mobile window, the start is from the top while on PC/Linux/OSX from
                        // bottom
                        if cfg!(target_os = "android") || cfg!(target_os = "ios") {
                            let window_height = windows.iter().next().unwrap().height();
                            location.y = window_height - location.y;
                        }
                        let mut touch = converters::convert_touch_input(*touch, location);

                        // We want to cancel any event when skip_window_event is true
                        if cfg!(feature = "gui") && skip_window_event {
                            touch.phase = bevy::input::touch::TouchPhase::Cancelled;
                            touch_input_events.send(touch);
                        } else {
                            touch_input_events.send(touch);
                        }
                    }
                    _ => (),
                }
            }
            _ => (),
        };

        if !skip_window_event {
            // Main events...
            match event {
                event::Event::WindowEvent {
                    event,
                    window_id: winit_window_id,
                    ..
                } => {
                    let mut state: SystemState<(
                        NonSendMut<BevyVulkanoWindows>,
                        Query<&mut Window>,
                        EventWriter<WindowResized>,
                        EventWriter<WindowFocused>,
                        EventWriter<WindowMoved>,
                        EventWriter<WindowCloseRequested>,
                        EventWriter<KeyboardInput>,
                        EventWriter<CursorMoved>,
                        EventWriter<CursorEntered>,
                        EventWriter<CursorLeft>,
                        EventWriter<MouseButtonInput>,
                        EventWriter<MouseWheel>,
                        EventWriter<ReceivedCharacter>,
                        EventWriter<WindowBackendScaleFactorChanged>,
                        EventWriter<WindowScaleFactorChanged>,
                        ResMut<Events<FileDragAndDrop>>,
                    )> = SystemState::from_world(&mut app.world);

                    let (
                        vulkano_winit_windows,
                        mut windows,
                        mut resize_events,
                        mut focused_events,
                        mut moved_events,
                        mut window_close_requested_events,
                        mut keyboard_input_events,
                        mut cursor_moved_events,
                        mut cursor_entered_events,
                        mut cursor_left_events,
                        mut mouse_button_input_events,
                        mut mouse_wheel_events,
                        mut received_character_events,
                        mut window_backend_scale_factor_changed_events,
                        mut window_scale_factor_changed_events,
                        mut file_drag_and_drop_events,
                    ) = state.get_mut(&mut app.world);

                    let window_entity = if let Some(window_id) =
                        vulkano_winit_windows.get_window_entity(winit_window_id)
                    {
                        window_id
                    } else {
                        warn!(
                            "Skipped event for unknown winit Window Id {:?}",
                            winit_window_id
                        );
                        return;
                    };

                    let mut window = if let Ok(window) = windows.get_mut(window_entity) {
                        window
                    } else {
                        warn!("Skipped event for unknown Window Id {:?}", winit_window_id);
                        return;
                    };

                    match event {
                        WindowEvent::Resized(size) => {
                            window
                                .resolution
                                .set_physical_resolution(size.width, size.height);

                            resize_events.send(WindowResized {
                                window: window_entity,
                                width: window.width(),
                                height: window.height(),
                            });
                        }
                        WindowEvent::CloseRequested => {
                            window_close_requested_events.send(WindowCloseRequested {
                                window: window_entity,
                            });
                        }
                        WindowEvent::KeyboardInput {
                            ref input, ..
                        } => {
                            keyboard_input_events.send(converters::convert_keyboard_input(input));
                        }
                        WindowEvent::CursorMoved {
                            position, ..
                        } => {
                            let winit_window = vulkano_winit_windows
                                .get_winit_window(window_entity)
                                .unwrap();
                            let inner_size = winit_window.inner_size();

                            // move origin to bottom left
                            let y_position = inner_size.height as f64 - position.y;

                            let physical_position = Vec2::new(position.x as f32, y_position as f32);
                            window.set_cursor_position(Some(physical_position));

                            cursor_moved_events.send(CursorMoved {
                                window: window_entity,
                                position: (physical_position.as_dvec2() / window.scale_factor())
                                    .as_vec2(),
                            });
                        }
                        WindowEvent::CursorEntered {
                            ..
                        } => {
                            cursor_entered_events.send(CursorEntered {
                                window: window_entity,
                            });
                        }
                        WindowEvent::CursorLeft {
                            ..
                        } => {
                            window.set_cursor_position(None);
                            cursor_left_events.send(CursorLeft {
                                window: window_entity,
                            });
                        }
                        WindowEvent::MouseInput {
                            state,
                            button,
                            ..
                        } => {
                            mouse_button_input_events.send(MouseButtonInput {
                                button: converters::convert_mouse_button(button),
                                state: converters::convert_element_state(state),
                            });
                        }
                        WindowEvent::MouseWheel {
                            delta, ..
                        } => match delta {
                            event::MouseScrollDelta::LineDelta(x, y) => {
                                mouse_wheel_events.send(MouseWheel {
                                    unit: MouseScrollUnit::Line,
                                    x,
                                    y,
                                });
                            }
                            event::MouseScrollDelta::PixelDelta(p) => {
                                mouse_wheel_events.send(MouseWheel {
                                    unit: MouseScrollUnit::Pixel,
                                    x: p.x as f32,
                                    y: p.y as f32,
                                });
                            }
                        },
                        WindowEvent::ReceivedCharacter(c) => {
                            received_character_events.send(ReceivedCharacter {
                                window: window_entity,
                                char: c,
                            })
                        }
                        WindowEvent::ScaleFactorChanged {
                            scale_factor,
                            new_inner_size,
                        } => {
                            window_backend_scale_factor_changed_events.send(
                                WindowBackendScaleFactorChanged {
                                    window: window_entity,
                                    scale_factor,
                                },
                            );

                            let prior_factor = window.scale_factor();
                            window.resolution.set_scale_factor(scale_factor);
                            let new_factor = window.scale_factor();

                            if let Some(forced_factor) = window.resolution.scale_factor_override() {
                                // If there is a scale factor override, then force that to be used
                                // Otherwise, use the OS suggested size
                                // We have already told the OS about our resize constraints, so
                                // the new_inner_size should take those into account
                                // *new_inner_size = winit::dpi::LogicalSize::new(
                                //     window.requested_width(),
                                //     window.requested_height(),
                                // )
                                // .to_physical::<u32>(forced_factor);
                            } else if approx::relative_ne!(new_factor, prior_factor) {
                                window_scale_factor_changed_events.send(WindowScaleFactorChanged {
                                    window: window_entity,
                                    scale_factor,
                                });
                            }

                            let new_logical_width = new_inner_size.width as f64 / new_factor;
                            let new_logical_height = new_inner_size.height as f64 / new_factor;
                            if approx::relative_ne!(window.width() as f64, new_logical_width)
                                || approx::relative_ne!(window.height() as f64, new_logical_height)
                            {
                                resize_events.send(WindowResized {
                                    window: window_entity,
                                    width: new_logical_width as f32,
                                    height: new_logical_height as f32,
                                });
                            }

                            window.resolution.set_physical_resolution(
                                new_inner_size.width,
                                new_inner_size.height,
                            )
                        }
                        WindowEvent::Focused(focused) => {
                            window.focused = focused;
                            focused_events.send(WindowFocused {
                                window: window_entity,
                                focused,
                            });
                        }
                        WindowEvent::DroppedFile(path_buf) => {
                            file_drag_and_drop_events.send(FileDragAndDrop::DroppedFile {
                                window: window_entity,
                                path_buf,
                            });
                        }
                        WindowEvent::HoveredFile(path_buf) => {
                            file_drag_and_drop_events.send(FileDragAndDrop::HoveredFile {
                                window: window_entity,
                                path_buf,
                            });
                        }
                        WindowEvent::HoveredFileCancelled => {
                            file_drag_and_drop_events.send(FileDragAndDrop::HoveredFileCancelled {
                                window: window_entity,
                            });
                        }
                        WindowEvent::Moved(position) => {
                            let position = ivec2(position.x, position.y);
                            window.position = bevy::prelude::WindowPosition::At(position);

                            moved_events.send(WindowMoved {
                                entity: window_entity,
                                position,
                            });
                        }
                        _ => {}
                    }
                }
                event::Event::DeviceEvent {
                    event:
                        DeviceEvent::MouseMotion {
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
                    handle_create_window_events(&mut app.world, event_loop);
                    if active {
                        app.update();
                    }
                }
                _ => (),
            }
        }
    };
    if should_return_from_run {
        let _exit_code = run_return(&mut event_loop, event_handler);
    } else {
        run(event_loop, event_handler);
    }
}

fn handle_create_window_events(world: &mut World, event_loop: &EventLoopWindowTarget<()>) {
    let mut handle_create_window_events_state: SystemState<(
        Commands,
        Res<BevyVulkanoContext>,
        NonSend<VulkanoWinitConfig>,
        NonSendMut<BevyVulkanoWindows>,
        Query<(Entity, &mut Window), Added<Window>>,
        EventWriter<WindowCreated>,
    )> = SystemState::from_world(world);

    let (
        mut commands,
        vulkano_context,
        vulkano_config,
        mut vulkano_winit_windows,
        mut new_windows,
        mut event_writer,
    ) = handle_create_window_events_state.get_mut(world);

    //TODO: Query<(Entity, &mut Window), Added<Window>> is suppose to react to only created windows, but it keeps
    // triggering each frame causing a window to be created constantly

    for (entity, create_window) in new_windows.iter_mut() {
        println!("Creating window: {:?}", create_window);

        // let window = vulkano_winit_windows.create_window(
        //     &mut commands,
        //     event_loop,
        //     entity,
        //     create_window,
        //     &vulkano_context.context,
        //     &vulkano_config,
        // );

        // commands.spawn(window);

        // event_writer.send(WindowCreated {
        //     window: entity,
        // });
    }

    handle_create_window_events_state.apply(world);
}

fn handle_initial_window_events(world: &mut World, event_loop: &EventLoop<()>) {
    let mut handle_initial_window_events_state: SystemState<(
        Commands,
        Res<BevyVulkanoContext>,
        NonSend<VulkanoWinitConfig>,
        NonSendMut<BevyVulkanoWindows>,
        Query<(Entity, &Window)>,
        EventWriter<WindowCreated>,
    )> = SystemState::from_world(world);

    let (
        mut commands,
        vulkano_context,
        vulkano_config,
        mut vulkano_winit_windows,
        new_windows,
        mut event_writer,
    ) = handle_initial_window_events_state.get_mut(world);

    for (entity, window) in new_windows.iter() {
        let window = vulkano_winit_windows.create_window(
            &mut commands,
            event_loop,
            entity,
            window,
            &vulkano_context.context,
            &vulkano_config,
        );

        commands.spawn(window);

        event_writer.send(WindowCreated {
            window: entity,
        });
    }

    handle_initial_window_events_state.apply(world);
}

pub fn exit_on_window_close_system(
    mut app_exit_events: EventWriter<AppExit>,
    mut windows: NonSendMut<BevyVulkanoWindows>,
    mut pipeline_data: ResMut<PipelineSyncData>,
    mut window_close_events: EventWriter<WindowClosed>,
    primary_window_entity: Query<Entity, With<PrimaryWindow>>,
    mut window_close_requested_events: EventReader<WindowCloseRequested>,
) {
    for event in window_close_requested_events.iter() {
        let (app_close, window_close) = close_window(
            event.window,
            &mut windows,
            primary_window_entity.get_single(),
            &mut pipeline_data,
        );

        if app_close {
            app_exit_events.send(AppExit);
        } else if window_close {
            window_close_events.send(WindowClosed {
                window: event.window,
            })
        }
    }
}

fn close_window(
    window_entity: bevy::prelude::Entity,
    windows: &mut BevyVulkanoWindows,
    primary_window_entity: Result<bevy::prelude::Entity, bevy::ecs::query::QuerySingleError>,
    pipeline_data: &mut PipelineSyncData,
    // App close?, Window was closed?
) -> (bool, bool) {
    // Close app on primary window exit
    if let Ok(primary_window) = primary_window_entity {
        if window_entity == primary_window {
            return (true, false);
        }
    } else {
        // primary window was closed
        return (true, false);
    }

    let winit_id = if let Some(winit_window) = windows.get_winit_window(window_entity) {
        winit_window.id()
    } else {
        // Window already closed
        return (false, false);
    };

    pipeline_data.remove(window_entity);
    windows.windows.remove(&winit_id);
    (false, true)
}

#[cfg(feature = "gui")]
pub fn begin_egui_frame_system(mut vulkano_windows: NonSendMut<BevyVulkanoWindows>) {
    for (_, (_, g)) in vulkano_windows.windows.iter_mut() {
        g.begin_frame();
    }
}
