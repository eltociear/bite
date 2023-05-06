use crate::gui::quad;
use crate::gui::texture::Texture;
use crate::gui::uniforms;
use crate::gui::Error;
use crate::gui::RenderContext;
use tokenizing::colors;

use std::mem::size_of;
use std::sync::atomic::Ordering;

use tokenizing::Token;
use wgpu_glyph::{GlyphBrush, GlyphBrushBuilder};
use winit::dpi::PhysicalSize;

pub struct Backend {
    pub size: winit::dpi::PhysicalSize<u32>,

    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,

    pub surface: wgpu::Surface,
    pub surface_cfg: wgpu::SurfaceConfiguration,
    pub pipeline: wgpu::RenderPipeline,
    pub bind_groups: Vec<wgpu::BindGroup>,
    pub vertex_buffers: Vec<wgpu::Buffer>,
    pub index_buffers: Vec<wgpu::Buffer>,
    pub index_count: u32,

    pub staging_belt: wgpu::util::StagingBelt,

    pub glyph_brush: GlyphBrush<()>,

    quad_pipeline: crate::gui::quad::Pipeline,
}

impl Backend {
    pub async fn new(window: &winit::window::Window) -> Result<Self, Error> {
        let backends = if cfg!(target_os = "windows") || cfg!(target_os = "linux") {
            wgpu::Backends::VULKAN
        } else {
            wgpu::Backends::METAL
        };

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            dx12_shader_compiler: wgpu::Dx12Compiler::Fxc,
        });

        let size = window.inner_size();
        let surface = unsafe { instance.create_surface(&window).map_err(Error::SurfaceCreation)? };

        let adapter = instance
            .enumerate_adapters(backends)
            .find(|adapter| adapter.is_surface_supported(&surface))
            .ok_or(Error::AdapterRequest)?;

        let device_desc = wgpu::DeviceDescriptor {
            label: Some("bite::ui device"),
            features: wgpu::Features::empty(),
            limits: wgpu::Limits::downlevel_defaults(),
        };

        let (device, queue) =
            adapter.request_device(&device_desc, None).await.map_err(Error::DeviceRequest)?;

        let surface_capabilities = surface.get_capabilities(&adapter);

        let alpha_mode = surface_capabilities.alpha_modes[0];

        let surface_format = {
            let default_format = surface_capabilities.formats[0];

            surface_capabilities
                .formats
                .into_iter()
                .find(|format| format.describe().srgb)
                .unwrap_or(default_format)
        };

        let surface_cfg = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: size.width,
            height: size.height,
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode,
            view_formats: Vec::new(),
        };

        surface.configure(&device, &surface_cfg);

        Texture::set_layout(
            &device,
            &wgpu::BindGroupLayoutDescriptor {
                label: Some("bite::ui texture bind group layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            },
        );

        let texture = Texture::new("./assets/joe_biden.png", &device, &queue).await?;

        let (vertices, indices) = uniforms::create_vertices();

        let vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bite::ui vertex buffer"),
            size: (size_of::<uniforms::Vertex>() * vertices.len()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        queue.write_buffer(&vertex_buffer, 0, bytemuck::cast_slice(&vertices[..]));

        let index_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bite::ui indices buffer"),
            size: (size_of::<u16>() * indices.len()) as u64,
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        queue.write_buffer(&index_buffer, 0, bytemuck::cast_slice(&indices[..]));

        // TODO: make module compiling run in parallel, not concurrently
        let now = std::time::Instant::now();
        let (vert_module, frag_module) = tokio::try_join!(
            super::utils::generate_vulkan_shader_module(
                "./shaders/vert.glsl",
                wgpu::ShaderStages::VERTEX,
                &device,
            ),
            super::utils::generate_vulkan_shader_module(
                "./shaders/frag.glsl",
                wgpu::ShaderStages::FRAGMENT,
                &device,
            )
        )?;

        println!("took {:#?} to generate shaders", now.elapsed());

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("bite::ui pipeline layout"),
            bind_group_layouts: &[Texture::layout()],
            push_constant_ranges: &[],
        });

        // Create the render pipelines. These describe how the data will flow through the GPU, and
        // what constraints and modifiers it will have.
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Primary pipeline"),
            layout: Some(&pipeline_layout),
            multiview: None,
            vertex: wgpu::VertexState {
                module: &vert_module,
                entry_point: "main",
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: size_of::<uniforms::Vertex>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &frag_module,
                entry_point: "main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            // How the triangles will be rasterized
            primitive: wgpu::PrimitiveState {
                // type of data we are passing in
                topology: wgpu::PrimitiveTopology::TriangleList,
                front_face: wgpu::FrontFace::Cw,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                // The number of samples for multisampling
                count: 1,
                // a mask for what samples are active: !0 means all of them
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
        });

        let staging_belt = wgpu::util::StagingBelt::new(1024);

        let font = include_bytes!("../../assets/LigaSFMonoNerdFont-Regular.otf");
        let font = ab_glyph::FontArc::try_from_slice(font).unwrap();
        let glyph_brush = GlyphBrushBuilder::using_font(font).build(&device, surface_format);

        let quad_pipeline = crate::gui::quad::Pipeline::new(&device, surface_format);

        Ok(Self {
            size,
            instance,
            adapter,
            device,
            queue,
            surface,
            surface_cfg,
            bind_groups: vec![texture.bind_group],
            pipeline,
            vertex_buffers: vec![vertex_buffer],
            index_buffers: vec![index_buffer],
            index_count: indices.len() as u32,
            staging_belt,
            glyph_brush,
            quad_pipeline,
        })
    }

    pub fn redraw(&mut self, ctx: &mut RenderContext) -> Result<(), Error> {
        let frame = self.surface.get_current_texture().map_err(Error::DrawTexture)?;
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("bite::ui encoder"),
        });

        let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 5.0 / 255.0,
                        g: 5.0 / 255.0,
                        b: 5.0 / 255.0,
                        a: 1.0,
                    }),
                    store: true,
                },
            })],
            depth_stencil_attachment: None,
        });

        render_pass.set_pipeline(&self.pipeline);
        render_pass.set_bind_group(0, &self.bind_groups[0], &[]);
        // render_pass.set_vertex_buffer(0, self.vertex_buffers[0].slice(..));
        // render_pass.set_index_buffer(self.index_buffers[0].slice(..), wgpu::IndexFormat::Uint16);
        // render_pass.draw_indexed(0..self.index_count, 0, 0..1);

        // required drop because render_pass and queue take a &wgpu::Device
        drop(render_pass);

        let font_size = ctx.scale_factor * ctx.font_size;

        // queue fps text
        self.glyph_brush.queue(wgpu_glyph::Section {
            screen_position: (ctx.scale_factor * 5.0, ctx.scale_factor * 5.0),
            bounds: (self.size.width as f32, self.size.height as f32),
            text: vec![wgpu_glyph::Text::new(&format!("FPS: {}", ctx.fps))
                .with_color(colors::WHITE)
                .with_scale(font_size)],
            ..wgpu_glyph::Section::default()
        });

        if ctx.show_donut.load(Ordering::Relaxed) {
            ctx.listing_offset = 0.0;

            // queue donut text
            self.glyph_brush.queue(wgpu_glyph::Section {
                screen_position: (self.size.width as f32 / 2.0, self.size.height as f32 / 2.0),
                layout: wgpu_glyph::Layout::default()
                    .h_align(wgpu_glyph::HorizontalAlign::Center)
                    .v_align(wgpu_glyph::VerticalAlign::Center),
                text: vec![wgpu_glyph::Text::new(&ctx.donut.frame)
                    .with_color(colors::WHITE)
                    .with_scale(ctx.scale_factor * 10.0)],
                ..wgpu_glyph::Section::default()
            });
        }

        // draw donut/fps
        self.glyph_brush
            .draw_queued(
                &self.device,
                &mut self.staging_belt,
                &mut encoder,
                &view,
                self.size.width,
                self.size.height,
            )
            .map_err(Error::DrawText)?;

        let lines_scrolled = (ctx.listing_offset / ctx.font_size) as usize;

        if let Some(ref dissasembly) = ctx.dissasembly {
            let mut text: Vec<Token> = Vec::new();
            let symbols = &dissasembly.symbols;
            let lines = dissasembly
                .proc
                .iter()
                .skip(lines_scrolled)
                .take((self.size.height as f32 / font_size).ceil() as usize);

            // for each instruction
            for (addr, inst) in lines {
                ctx.show_donut.store(false, Ordering::Relaxed);

                // if the address matches a symbol, print it
                if let Some(label) = symbols.get_by_addr(addr) {
                    text.push(Token::from_str("\n<", &colors::BLUE));
                    for token in label.tokens() {
                        text.push(token.as_ref());
                    }

                    text.push(Token::from_str(">:\n", &colors::BLUE));
                }

                // memory address
                text.push(Token::from_string(
                    format!("0x{addr:0>10X}  "),
                    &colors::GRAY40,
                ));

                // instruction's bytes
                text.push(Token::from_string(
                    inst.bytes(dissasembly.proc.as_ref(), addr),
                    &colors::GREEN,
                ));

                for token in inst.tokens().iter() {
                    text.push(token.clone());
                }

                text.push(Token::from_str("\n", &colors::WHITE));
            }

            // queue assembly listing text
            self.glyph_brush.queue(wgpu_glyph::Section {
                screen_position: (ctx.scale_factor * 5.0, font_size * 1.5),
                text: text.iter().map(|t| t.text(font_size)).collect(),
                ..wgpu_glyph::Section::default()
            });

            // orthogonal projection
            let proj = glam::mat4(
                glam::vec4(2.0 / self.size.width as f32, 0.0, 0.0, 0.0),
                glam::vec4(0.0, -2.0 / self.size.height as f32, 0.0, 0.0),
                glam::vec4(0.0, 0.0, 1.0, 0.0),
                glam::vec4(-1.0, 1.0, 0.0, 1.0),
            );

            // draw assembly listing
            self.glyph_brush
                .draw_queued_with_transform(
                    &self.device,
                    &mut self.staging_belt,
                    &mut encoder,
                    &view,
                    proj.to_cols_array(),
                )
                .map_err(Error::DrawText)?;

            let len = dissasembly.proc.iter().size_hint().0;
            let bar_height =
                (self.size.height * self.size.height) as f32 / (len as f32 * font_size);
            let bar_height = bar_height.max(font_size);
            let offset = ctx.listing_offset / (len as f32 * font_size);
            let screen_offset = offset * (self.size.height as f32 - bar_height);

            let instances = [
                quad::Instance {
                    position: [self.size.width as f32 - font_size / 2.0, 0.0],
                    size: [font_size / 3.0, self.size.height as f32],
                    color: colors::GRAY10,
                },
                quad::Instance {
                    position: [self.size.width as f32 - font_size / 2.0, screen_offset],
                    size: [font_size / 3.0, bar_height],
                    color: colors::GRAY99,
                },
            ];

            self.quad_pipeline.draw(
                &mut encoder,
                &instances,
                &self.device,
                &view,
                &mut self.staging_belt,
                self.size,
            );
        }

        // submit work
        self.staging_belt.finish();
        self.queue.submit(Some(encoder.finish()));

        // schedule texture to be renderer on surface
        frame.present();

        // recall unused staging buffers
        self.staging_belt.recall();

        Ok(())
    }

    pub fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width > 0 && size.height > 0 {
            self.size = size;
            self.surface_cfg.width = size.width;
            self.surface_cfg.height = size.height;
            self.surface.configure(&self.device, &self.surface_cfg);
        }
    }
}
