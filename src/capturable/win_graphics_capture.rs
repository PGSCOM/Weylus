//! Windows Graphics Capture API implementation for Weylus
//! 
//! This module provides screen capture functionality using the modern Windows Graphics Capture API
//! which is more robust than older methods like GDI or DXGI Desktop Duplication.

#![cfg(windows)]

use crate::capturable::{Capturable, Geometry, Recorder};
use crate::video::PixelProvider;
use std::boxed::Box;
use std::error::Error;
use std::sync::{
    mpsc::{channel, Receiver as StdReceiver, Sender as StdSender},
    Arc, Mutex,
};
use tracing::{debug, error, warn};

// Windows Crate Imports
use windows::{
    core::{ComInterface, Result, HSTRING, PCWSTR},
    Foundation::TypedEventHandler,
    Graphics::{
        Capture::{
            Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem,
            GraphicsCaptureSession, 
        },
        DirectX::Direct3D11::IDirect3DDevice,
        DirectX::DXGI_FORMAT_B8G8R8A8_UNORM,
    },
    Win32::{
        Foundation::{HWND, RECT, TRUE},
        Graphics::{
            Direct3D11::{
                ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
                D3D11_CPU_ACCESS_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ,
                D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, D3D11_SDK_VERSION,
                D3D11CreateDevice, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_DRIVER_TYPE_HARDWARE,
                D3D11_FEATURE_LEVEL_11_0,
            },
            Dxgi::{DXGI_ERROR_DEVICE_REMOVED},
        },
        System::{
            WinRT::{
                RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED,
                IGraphicsCaptureItemInterop,
            },
        },
        UI::WindowsAndMessaging::{GetDesktopWindow, GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN},
    },
};

#[derive(Debug)]
pub struct WGCError(String);

impl std::fmt::Display for WGCError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "WindowsGraphicsCapture Error: {}", self.0)
    }
}

impl Error for WGCError {}

// Estado compartido entre el hilo de captura y el callback
struct CaptureLoopState {
    frame_sender: StdSender<Result<Arc<Mutex<Vec<u8>>>>>,
    d3d_device: ID3D11Device,
    d3d_context: ID3D11DeviceContext,
    staging_texture: Option<ID3D11Texture2D>,
    buffer_width: u32,
    buffer_height: u32,
}

#[derive(Clone)]
pub struct WinGraphicsCaptureCapturable {
    item: GraphicsCaptureItem,
    name: String,
    item_width: u32,
    item_height: u32,
    hwnd: HWND,
}

impl WinGraphicsCaptureCapturable {
    /// Crea una nueva instancia para capturar el monitor primario
    pub fn new_primary_monitor() -> Result<Self> {
        unsafe { RoInitialize(RO_INIT_MULTITHREADED)? };

        let desktop_hwnd = unsafe { GetDesktopWindow() };
        let interop: IGraphicsCaptureItemInterop = windows::core::factory()?;
        let item = unsafe { interop.CreateForWindow(desktop_hwnd)? };

        let size = item.Size()?;
        let name = item.DisplayName()?.to_string_lossy();

        Ok(Self {
            item,
            name,
            item_width: size.Width as u32,
            item_height: size.Height as u32,
            hwnd: desktop_hwnd,
        })
    }
}

impl Capturable for WinGraphicsCaptureCapturable {
    fn name(&self) -> String {
        format!("Desktop {} (WGC)", self.name)
    }

    fn geometry(&self) -> Result<Geometry, Box<dyn Error>> {
        let x = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
        let y = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };

        // Para el escritorio completo
        if self.hwnd == unsafe { GetDesktopWindow() } {
            Ok(Geometry::VirtualScreen(0, 0, self.item_width, self.item_height, x, y))
        } else {
            // Para ventanas específicas (futuro)
            Ok(Geometry::VirtualScreen(0, 0, self.item_width, self.item_height, x, y))
        }
    }

    fn before_input(&mut self) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    fn recorder(&self, capture_cursor: bool) -> Result<Box<dyn Recorder>, Box<dyn Error>> {
        Ok(Box::new(WinGraphicsCaptureRecorder::new(
            self.item.clone(),
            capture_cursor,
        )?))
    }
}

impl Drop for WinGraphicsCaptureCapturable {
    fn drop(&mut self) {
        unsafe { RoUninitialize() };
    }
}

pub struct WinGraphicsCaptureRecorder {
    frame_receiver: StdReceiver<Result<Arc<Mutex<Vec<u8>>>>>,
    pixel_buffer: Arc<Mutex<Vec<u8>>>,
    _session: GraphicsCaptureSession,
    _frame_pool: Direct3D11CaptureFramePool,
    width: u32,
    height: u32,
    _com_thread_init: ComThreadInit,
}

// Helper para inicializar/desinicializar COM en un hilo
struct ComThreadInit;

impl ComThreadInit {
    fn new() -> Result<Self> {
        unsafe { RoInitialize(RO_INIT_MULTITHREADED)? };
        Ok(ComThreadInit)
    }
}

impl Drop for ComThreadInit {
    fn drop(&mut self) {
        unsafe { RoUninitialize() };
    }
}

impl WinGraphicsCaptureRecorder {
    pub fn new(item: GraphicsCaptureItem, capture_cursor: bool) -> Result<Self> {
        let com_init = ComThreadInit::new()?;

        let (d3d_device, d3d_context) = create_d3d_device_and_context()?;
        let direct3d_device: IDirect3DDevice = d3d_device.cast()?;

        let item_size = item.Size()?;
        let width = item_size.Width as u32;
        let height = item_size.Height as u32;

        let frame_pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &direct3d_device,
            DXGI_FORMAT_B8G8R8A8_UNORM,
            2, // Número de búferes
            &item_size,
        )?;

        let session = frame_pool.CreateCaptureSession(&item)?;
        session.SetIsCursorCaptureEnabled(capture_cursor)?;

        let (frame_sender, frame_receiver) = channel();
        let initial_buffer_size = (width * height * 4) as usize; // BGRA8
        let pixel_buffer = Arc::new(Mutex::new(vec![0u8; initial_buffer_size]));

        let capture_loop_state = Arc::new(Mutex::new(CaptureLoopState {
            frame_sender,
            d3d_device,
            d3d_context,
            staging_texture: None,
            buffer_width: 0,
            buffer_height: 0,
        }));

        let frame_pool_clone = frame_pool.clone();
        frame_pool.FrameArrived(
            &TypedEventHandler::new(
                move |pool: &Option<Direct3D11CaptureFramePool>, _| {
                    if let Some(pool) = pool {
                        match pool.TryGetNextFrame() {
                            Ok(frame) => {
                                let mut state_guard = capture_loop_state.lock().expect("Mutex poisoned");
                                match process_frame(&frame, &mut state_guard) {
                                    Ok(pixel_data_vec) => {
                                        let _ = state_guard.frame_sender.send(Ok(Arc::new(Mutex::new(pixel_data_vec))));
                                    }
                                    Err(e) => {
                                        warn!("Error processing frame: {:?}", e);
                                        let _ = state_guard.frame_sender.send(Err(e));
                                    }
                                }
                            }
                            Err(e) => {
                                if e.code() != DXGI_ERROR_DEVICE_REMOVED {
                                    warn!("TryGetNextFrame error: {:?}", e);
                                }
                                let mut state_guard = capture_loop_state.lock().expect("Mutex poisoned");
                                let _ = state_guard.frame_sender.send(Err(e.into()));
                            }
                        }
                    }
                    Ok(())
                }
            )
        )?;

        session.StartCapture()?;
        debug!("Windows Graphics Capture session started for: {}", item.DisplayName()?);

        Ok(Self {
            frame_receiver,
            pixel_buffer,
            _session: session,
            _frame_pool: frame_pool_clone,
            width,
            height,
            _com_thread_init: com_init,
        })
    }
}

fn create_d3d_device_and_context() -> Result<(ID3D11Device, ID3D11DeviceContext)> {
    let mut d3d_device: Option<ID3D11Device> = None;
    let mut d3d_context: Option<ID3D11DeviceContext> = None;
    let feature_levels = [D3D11_FEATURE_LEVEL_11_0];

    unsafe {
        D3D11CreateDevice(
            None, // Adaptador por defecto
            D3D11_DRIVER_TYPE_HARDWARE,
            None, // Software module
            D3D11_CREATE_DEVICE_BGRA_SUPPORT, // Necesario para DXGI_FORMAT_B8G8R8A8_UNORM
            Some(&feature_levels),
            D3D11_SDK_VERSION,
            Some(&mut d3d_device),
            None, // Actual feature level
            Some(&mut d3d_context),
        )?;
    }
    Ok((d3d_device.unwrap(), d3d_context.unwrap()))
}

fn process_frame(frame: &Direct3D11CaptureFrame, state: &mut CaptureLoopState) -> Result<Vec<u8>> {
    let surface_texture: ID3D11Texture2D = frame.Surface()?.cast()?;
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { surface_texture.GetDesc(&mut desc) };

    // Asegurar que la textura de staging exista y tenga el tamaño correcto
    if state.staging_texture.is_none() || state.buffer_width != desc.Width || state.buffer_height != desc.Height {
        debug!("Creating/Resizing staging texture to {}x{}", desc.Width, desc.Height);
        let mut staging_desc = desc;
        staging_desc.Usage = D3D11_USAGE_STAGING;
        staging_desc.BindFlags = 0;
        staging_desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        staging_desc.MiscFlags = 0;

        state.staging_texture = Some(unsafe { state.d3d_device.CreateTexture2D(&staging_desc, None)? });
        state.buffer_width = desc.Width;
        state.buffer_height = desc.Height;
    }

    let staging_texture = state.staging_texture.as_ref().unwrap();
    let mut pixel_data_vec: Vec<u8> = vec![0u8; (desc.Width * desc.Height * 4) as usize]; // BGRA    unsafe {
        state.d3d_context.CopyResource(Some(staging_texture), Some(&surface_texture));

        let mut mapped_surface = D3D11_MAPPED_SUBRESOURCE::default();
        state.d3d_context.Map(Some(staging_texture), 0, D3D11_MAP_READ, 0, Some(&mut mapped_surface))?;

        let source_slice = std::slice::from_raw_parts(
            mapped_surface.pData as *const u8,
            (desc.Height * mapped_surface.RowPitch) as usize,
        );

        let bytes_per_pixel = 4; // Para BGRA
        let dest_row_pitch = desc.Width * bytes_per_pixel;

        for row in 0..desc.Height {
            let src_offset = (row * mapped_surface.RowPitch) as usize;
            let dest_offset = (row * dest_row_pitch) as usize;
            let src_row = &source_slice[src_offset..(src_offset + dest_row_pitch as usize)];
            pixel_data_vec[dest_offset..(dest_offset + dest_row_pitch as usize)].copy_from_slice(src_row);
        }

        state.d3d_context.Unmap(Some(staging_texture), 0);
    }
    Ok(pixel_data_vec)
}

impl Recorder for WinGraphicsCaptureRecorder {
    fn capture(&mut self) -> Result<PixelProvider, Box<dyn Error>> {
        match self.frame_receiver.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(Ok(frame_data_arc)) => {
                let frame_data_guard = frame_data_arc.lock().map_err(|e| Box::new(WGCError(format!("Mutex poisoned: {}", e))))?;
                
                // Actualizar el último frame bueno
                {
                    let mut last_good_frame = self.pixel_buffer.lock().map_err(|e| Box::new(WGCError(format!("Mutex for last good frame poisoned: {}", e))))?;
                    last_good_frame.clear();
                    last_good_frame.extend_from_slice(&frame_data_guard);
                }
                
                Ok(PixelProvider::BGR0S(
                    self.width as usize,
                    self.height as usize,
                    (self.width * 4) as usize, // Stride para BGRA
                    &frame_data_guard,
                ))
            }
            Ok(Err(e)) => {
                warn!("Error received from frame processing thread: {:?}", e);
                // Intentar devolver el último frame bueno
                let last_good_frame = self.pixel_buffer.lock().map_err(|e| Box::new(WGCError(format!("Mutex for last good frame poisoned: {}", e))))?;
                if !last_good_frame.is_empty() {
                    Ok(PixelProvider::BGR0S(self.width as usize, self.height as usize, (self.width * 4) as usize, &last_good_frame))
                } else {
                    Err(Box::new(WGCError(format!("Frame processing error and no last good frame: {:?}", e))))
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                warn!("Timeout receiving frame from WGC callback.");
                // Intentar devolver el último frame bueno
                let last_good_frame = self.pixel_buffer.lock().map_err(|e| Box::new(WGCError(format!("Mutex for last good frame poisoned on timeout: {}", e))))?;
                if !last_good_frame.is_empty() {
                    Ok(PixelProvider::BGR0S(self.width as usize, self.height as usize, (self.width * 4) as usize, &last_good_frame))
                } else {
                    Err(Box::new(WGCError("Timeout and no last good frame available".into())))
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Err(Box::new(WGCError("Frame processing thread disconnected".into())))
            }
        }
    }
}

impl Drop for WinGraphicsCaptureRecorder {
    fn drop(&mut self) {
        debug!("Dropping WinGraphicsCaptureRecorder, closing session.");
        // La sesión se cierra automáticamente cuando _session y _frame_pool se dropean
    }
}
