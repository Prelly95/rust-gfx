use std::mem::ManuallyDrop;
use std::borrow::Borrow;
use shaderc::ShaderKind;

use winit::event_loop::ControlFlow;
use winit::dpi::
{	LogicalSize,
	PhysicalSize
};
use winit::event::
{	Event,
	WindowEvent
};

use gfx_backend_vulkan as backend;

use gfx_hal::Instance;
use gfx_hal::adapter	::PhysicalDevice;
use gfx_hal::device		::Device;
use gfx_hal::queue::
{	QueueFamily,
	CommandQueue,
	Submission,
};
use gfx_hal::command::
{	Level,
	ClearColor,
	ClearValue,
	CommandBuffer,
	CommandBufferFlags,
	SubpassContents,
};
use gfx_hal::image::
{	Layout,
	Extent
};
use gfx_hal::window::
{	Extent2D,
	PresentationSurface,
	Surface,
	SwapchainConfig
};
use gfx_hal::pool::
	{	CommandPool,
		CommandPoolCreateFlags
	};
use gfx_hal::format::
	{	ChannelType,
		Format
	};
use gfx_hal::pass::
	{	Attachment,
		AttachmentLoadOp,
		AttachmentOps,
		AttachmentStoreOp,
		SubpassDesc,
		Subpass
	};
use gfx_hal::pso::
{	BlendState,
	ColorBlendDesc,
	ColorMask,
	EntryPoint,
	Face,
	GraphicsPipelineDesc,
	InputAssemblerDesc,
	Primitive,
	PrimitiveAssemblerDesc,
	Rasterizer,
	Specialization,
	Rect,
	Viewport,
	ShaderStageFlags,
};

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct PushConstants {
	color: [f32; 4],
	pos: [f32; 2],
	scale: [f32; 2],
}


fn main()
{	const APP_NAME: &'static str = "Rust GFX";
	const WINDOW_SIZE: [u32; 2] = [512, 512];

	let event_loop = winit::event_loop::EventLoop::new();
	let mut should_configure_swapchain = true;

	let (logical_window_size, physical_window_size) =
	{	let dpi = event_loop.primary_monitor().scale_factor();
		let logical: LogicalSize<u32> = WINDOW_SIZE.into();
		let physical: PhysicalSize<u32> = logical.to_physical(dpi);
		(logical, physical)
	};

	let mut surface_extent = Extent2D
	{	width: physical_window_size.width,
		height: physical_window_size.height,
	};

	let window = winit::window::WindowBuilder::new()
		.with_title(APP_NAME)
		.with_inner_size(logical_window_size)
		.build(&event_loop)
		.expect("failed to create window");

	let (instance, surface, adapter) =
	{	let instance = backend::Instance::create(APP_NAME, 1).expect("Backend not supported");
		let surface = unsafe
		{	instance
				.create_surface(&window)
				.expect("Failed to creare surface for window")
		};

		let adapter = instance.enumerate_adapters().remove(0);

		(instance, surface, adapter)
	};

	let (device, mut queue_group) = 
	{	let queue_family = adapter
			.queue_families
			.iter()
			.find(|family|
			{	surface.supports_queue_family(family) && family.queue_type().supports_graphics()
			})
			.expect("No compatible queue family found");
		
		let mut gpu = unsafe
		{	adapter
				.physical_device
				.open(&[(queue_family, &[1.0])], gfx_hal::Features::empty())
				.expect("Failed to open device")
		};

		(gpu.device, gpu.queue_groups.pop().unwrap())
	};
	
	let (command_pool, mut command_buffer) = unsafe
	{	let mut command_pool = device
			.create_command_pool(queue_group.family, CommandPoolCreateFlags::empty())
			.expect("Out of memory");
		
		let command_buffer = command_pool.allocate_one(Level::Primary);
		
		(command_pool, command_buffer)
	};
	
	let surface_color_format = 
	{	let supported_formats = surface
			.supported_formats(&adapter.physical_device)
			.unwrap_or(vec![]);
		
		let default_format = *supported_formats.get(0).unwrap_or(&Format::Rgba8Srgb);
		supported_formats
			.into_iter()
			.find(|format| format.base_format().1 == ChannelType::Srgb)
			.unwrap_or(default_format)
	};

	let render_pass =
	{	let color_attachment = Attachment
		{	format: Some(surface_color_format),
			samples: 1,
			ops: AttachmentOps::new(AttachmentLoadOp::Clear, AttachmentStoreOp::Store),
			stencil_ops: AttachmentOps::DONT_CARE,
			layouts: Layout::Undefined..Layout::Present,
		};

		let subpass = SubpassDesc
		{	colors: &[(0, Layout::ColorAttachmentOptimal)],
			depth_stencil: None,
			inputs: &[],
			resolves: &[],
			preserves: &[],
		};

		unsafe {
			device
				.create_render_pass(&[color_attachment], &[subpass], &[])
				.expect("Out of memory")
		}
	};

	let pipeline_layout = unsafe
	{	let push_constant_bytes = std::mem::size_of::<PushConstants>() as u32;
		device
			.create_pipeline_layout(&[], &[(ShaderStageFlags::VERTEX, 0..push_constant_bytes)])
			.expect("Out of memory")
	};

	let vertex_shader = include_str!("shaders/triangle.vert");
	let fragment_shader = include_str!("shaders/triangle.frag");

	let pipeline = unsafe
	{	make_pipeline::<backend::Backend>
		(	&device,
			&render_pass,
			&pipeline_layout,
			vertex_shader,
			fragment_shader,
		)
	};

	let submission_complete_fence = device.create_fence(true).expect("Out of memory");
	let rendering_complete_semaphore = device.create_semaphore().expect("Out of memory");

	struct Resources<B: gfx_hal::Backend>
	{	instance: B::Instance,
		surface: B::Surface,
		device: B::Device,
		render_passes: Vec<B::RenderPass>,
		pipeline_layouts: Vec<B::PipelineLayout>,
		pipelines: Vec<B::GraphicsPipeline>,
		command_pool: B::CommandPool,
		submission_complete_fence: B::Fence,
		rendering_complete_semaphore: B::Semaphore,
	}

	struct ResourceHolder<B: gfx_hal::Backend>(ManuallyDrop<Resources<B>>);

	impl<B: gfx_hal::Backend> Drop for ResourceHolder<B> {
		fn drop(&mut self) {
			unsafe {
				let Resources {
					instance,
					mut surface,
					device,
					command_pool,
					render_passes,
					pipeline_layouts,
					pipelines,
					submission_complete_fence,
					rendering_complete_semaphore,
				} = ManuallyDrop::take(&mut self.0);

				device.destroy_semaphore(rendering_complete_semaphore);
				device.destroy_fence(submission_complete_fence);
				for pipeline in pipelines {
					device.destroy_graphics_pipeline(pipeline);
				}
				for pipeline_layout in pipeline_layouts {
					device.destroy_pipeline_layout(pipeline_layout);
				}
				for render_pass in render_passes {
					device.destroy_render_pass(render_pass);
				}
				device.destroy_command_pool(command_pool);
				surface.unconfigure_swapchain(&device);
				instance.destroy_surface(surface);
			}
		}
	}

	let mut resource_holder: ResourceHolder<backend::Backend> = ResourceHolder
	(	ManuallyDrop::new
		(	Resources
			{	instance,
				surface,
				device,
				command_pool,
				render_passes: vec![render_pass],
				pipeline_layouts: vec![pipeline_layout],
				pipelines: vec![pipeline],
				submission_complete_fence,
				rendering_complete_semaphore,	
			}
		)
	);

	let start_time = std::time::Instant::now();
	event_loop.run(move | event, _, control_flow|
	{	match event
		{	Event::WindowEvent
			{	event, ..
			} => match event
			{	WindowEvent::CloseRequested => *control_flow = ControlFlow::Exit,
				WindowEvent::Resized(dims) =>
				{	surface_extent = Extent2D
					{	width: dims.width,
						height: dims.height,
					};
					should_configure_swapchain = true;
				}
				WindowEvent::ScaleFactorChanged
				{	new_inner_size, ..
				} =>
				{	surface_extent = Extent2D
					{	width: new_inner_size.width,
						height: new_inner_size.height,
					};
					should_configure_swapchain = true;
				}
				_ => (),
			},
			Event::MainEventsCleared => window.request_redraw(),
			Event::RedrawRequested(_) =>
			{	let res: &mut Resources<_> = &mut resource_holder.0;
				let render_pass = &res.render_passes[0];
				let pipeline_layout = &res.pipeline_layouts[0];
				let pipeline = &res.pipelines[0];

				let anim = start_time.elapsed().as_secs_f32().sin() * 0.5 + 0.5;
				let small = [0.33, 0.33];
				let triangles =
				&[	// Red triangle
					PushConstants
					{	color: [1.0, 0.0, 0.0, 1.0],
						pos: [-0.5, -0.5],
						scale: small,
					},
					// Green triangle
					PushConstants
					{	color: [0.0, 1.0, 0.0, 1.0],
						pos: [0.0, -0.5],
						scale: small,
					},
					// Blue triangle
					PushConstants
					{	color: [0.0, 0.0, 1.0, 1.0],
						pos: [0.5, -0.5],
						scale: small,
					},
					// Blue <-> cyan animated triangle
					PushConstants
					{	color: [0.0, anim, 1.0, 1.0],
						pos: [-0.5, 0.5],
						scale: small,
					},
					// Down <-> up animated triangle
					PushConstants
					{	color: [1.0, 1.0, 1.0, 1.0],
						pos: [0.0, 0.5 - anim * 0.5],
						scale: small,
					},
					// Small <-> big animated triangle
					PushConstants
					{	color: [1.0, 1.0, 1.0, 1.0],
						pos: [0.5, 0.5],
						scale: [0.33 + anim * 0.33, 0.33 + anim * 0.33],
					},
				];

				unsafe
				{	let render_timeout_ns = 1_000_000_000;
					res.device
						.wait_for_fence(&res.submission_complete_fence, render_timeout_ns)
						.expect("Out of memory or lost device");
					
					res.device
						.reset_fence(&res.submission_complete_fence)
						.expect("Out of memory");

					res.command_pool.reset(false);
				}
				if should_configure_swapchain
				{	let caps = res.surface.capabilities(&adapter.physical_device);
					let mut swapchain_config = 
						SwapchainConfig::from_caps(&caps, surface_color_format, surface_extent);
					
					if caps.image_count.contains(&3)
					{	swapchain_config.image_count = 3;
					}

					surface_extent = swapchain_config.extent;
					unsafe
					{	res.surface
							.configure_swapchain(&res.device, swapchain_config)
							.expect("Failed to configure swapchain");
					};
					should_configure_swapchain = false;
				}

				let surface_image = unsafe
				{	let acquire_timeout_ns = 1_000_000_000;
					match res.surface.acquire_image(acquire_timeout_ns)
					{	Ok((image, _)) => image,
						Err(_) =>
						{	should_configure_swapchain = true;
							return;
						}
					}
				};

				let framebuffer = unsafe
				{	res.device.create_framebuffer
					(	render_pass,
						vec![surface_image.borrow()],
						Extent
						{	width: surface_extent.width,
							height: surface_extent.height,
							depth: 1,
						}
					)
					.unwrap()
				};
				let viewport = 
				{
					Viewport
					{
						rect: Rect
						{
							x: 0,
							y: 0,
							w: surface_extent.width as i16,
							h: surface_extent.height as i16,
						},
						depth: 0.0..1.0,
					}
				};

				unsafe
				{	command_buffer.begin_primary(CommandBufferFlags::ONE_TIME_SUBMIT);
					
					command_buffer. set_viewports(0, &[viewport.clone()]);
					command_buffer.set_scissors(0, &[viewport.rect]);

					command_buffer.begin_render_pass
					(
						render_pass,
						&framebuffer,
						viewport.rect,
						&[ClearValue
						{	color: ClearColor
							{	float32: [0.0, 0.0, 0.0, 1.0],
							},
						}],
						SubpassContents::Inline,
					);
					command_buffer.bind_graphics_pipeline(pipeline);
					
					for triangle in triangles
					{	command_buffer.push_graphics_constants
						(	pipeline_layout,
							ShaderStageFlags::VERTEX,
							0,
							push_constant_bytes(triangle),
						);
						command_buffer.draw(0..3, 0..1);
					}

					command_buffer.end_render_pass();
					command_buffer.finish();

					// unsafe
					// {	
						let submission = Submission
						{	command_buffers: vec![&command_buffer],
							wait_semaphores: None,
							signal_semaphores: vec![&res.rendering_complete_semaphore],
						};
						queue_group.queues[0].submit(submission, Some(&res.submission_complete_fence));
						let result = queue_group.queues[0].present
						(	&mut res.surface,
							surface_image,
							Some(&res.rendering_complete_semaphore),
						);
						should_configure_swapchain |= result.is_err();
						res.device.destroy_framebuffer(framebuffer);
					// }
				}
			},
			_ => (),
		}

	});
}

fn compile_shader(glsl: &str, shader_kind: ShaderKind) -> Vec<u32>
{	let mut compiler = shaderc::Compiler::new().unwrap();

	let compiled_shader = compiler
		.compile_into_spirv(glsl, shader_kind, "unnamed", "main", None)
		.expect("Failed to compile shader");

	compiled_shader.as_binary().to_vec()
	
}

unsafe fn make_pipeline<B: gfx_hal::Backend>(
	device: &B::Device,
	render_pass: &B::RenderPass,
	pipeline_layout: &B::PipelineLayout,
	vertex_shader: &str,
	fragment_shader: &str,
) -> B::GraphicsPipeline
{	let vertex_shader_module = device
		.create_shader_module(&compile_shader(vertex_shader, ShaderKind::Vertex))
		.expect("Failed to create vertex shader modile");

	let fragment_shader_module = device
		.create_shader_module(&compile_shader(fragment_shader, ShaderKind::Fragment))
		.expect("Failed to create fragment shader module");

	let (vs_entry, fs_entry) =
	(	EntryPoint
		{	entry: "main",
			module: &vertex_shader_module,
			specialization: Specialization::default(),
		},
		EntryPoint
		{
			entry: "main",
			module: &fragment_shader_module,
			specialization: Specialization::default()
		}
	);

	let primitive_assembler = PrimitiveAssemblerDesc::Vertex
	{	buffers: &[],
		attributes: &[],
		input_assembler: InputAssemblerDesc::new(Primitive::TriangleList),
		vertex: vs_entry,
		tessellation: None,
		geometry: None,
	};

	let mut pipeline_desc = GraphicsPipelineDesc::new
	(	primitive_assembler,
		Rasterizer
		{	cull_face: Face::BACK,
			..Rasterizer::FILL
		},
		Some(fs_entry),
		pipeline_layout,
		Subpass
		{	index: 0,
			main_pass: render_pass,
		},
	);

	pipeline_desc.blender.targets.push(ColorBlendDesc
	{	mask: ColorMask::ALL,
		blend: Some(BlendState::ALPHA),
	});

	let pipeline = device
		.create_graphics_pipeline(&pipeline_desc, None)
		.expect("Failed to create graphics");

	device.destroy_shader_module(vertex_shader_module);
	device.destroy_shader_module(fragment_shader_module);

	pipeline
}

/// Returns a view of a struct as a slice of `u32`s.
unsafe fn push_constant_bytes<T>(push_constants: &T) -> &[u32] {
	let size_in_bytes = std::mem::size_of::<T>();
	let size_in_u32s = size_in_bytes / std::mem::size_of::<u32>();
	let start_ptr = push_constants as *const T as *const u32;
	std::slice::from_raw_parts(start_ptr, size_in_u32s)
}