# Implementación de Windows Graphics Capture API en Weylus

## Resumen de cambios realizados

Este documento describe la implementación de la Windows Graphics Capture API como reemplazo de la biblioteca `captrs` en Weylus para Windows.

### Archivos modificados:

1. **Cargo.toml** - Añadidas dependencias para Windows Graphics Capture API
2. **src/capturable/mod.rs** - Actualizado para usar la nueva implementación como principal, con fallback a captrs
3. **src/capturable/win_graphics_capture.rs** - Nuevo módulo con la implementación completa

### Características implementadas:

#### Windows Graphics Capture API
- ✅ Captura asíncrona de pantalla usando la API moderna de Windows
- ✅ Soporte para Direct3D 11 y texturas GPU
- ✅ Copiar texturas de GPU a CPU para codificación de video
- ✅ Manejo automático de cambios de resolución
- ✅ Captura del cursor opcional
- ✅ Gestión robusta de errores y timeouts
- ✅ Fallback automático a captrs si WGC falla

#### Ventajas sobre captrs:
- **Mejor rendimiento**: Usa la API nativa moderna de Windows
- **Más estable**: Menos problemas con aplicaciones UWP y juegos
- **Mejor compatibilidad**: Funciona con aplicaciones protegidas
- **Soporte futuro**: API oficialmente soportada por Microsoft

### Detalles técnicos:

#### Arquitectura asíncrona:
- Los frames llegan a través de callbacks de WinRT
- Canal `mpsc` para comunicación entre hilos
- Buffer de último frame conocido para casos de timeout
- Gestión automática de recursos COM/WinRT

#### Formato de píxeles:
- Captura en formato `DXGI_FORMAT_B8G8R8A8_UNORM` (BGRA)
- Conversión automática para compatibilidad con FFmpeg
- Manejo correcto de stride/pitch para texturas

#### Gestión de memoria:
- Textura de staging para acceso CPU
- Buffer circular para frames
- Liberación automática de recursos D3D11

### Uso:

El código detecta automáticamente si Windows Graphics Capture está disponible:

1. **Éxito WGC**: Usa la nueva API moderna
2. **Fallo WGC**: Fallback automático a captrs (comportamiento anterior)

No se requieren cambios en el código del usuario.

### Requisitos:

- Windows 10 versión 1903 (19H1) o superior
- Direct3D 11 compatible
- Rust con características de windows crate habilitadas

### Limitaciones conocidas:

1. **Solo monitor primario**: La implementación actual solo captura el monitor primario
2. **Formato fijo**: Solo BGRA, aunque es el más común
3. **Sin picker**: No incluye interfaz de selección de ventana (por simplicidad)

### Futuras mejoras posibles:

1. **Multi-monitor**: Soporte para múltiples monitores
2. **Selector de ventana**: UI para elegir qué capturar
3. **GPU encoding**: Integración directa con codificadores de hardware
4. **HDR**: Soporte para contenido HDR
5. **Optimizaciones**: Reducir copias CPU<->GPU

### Notas de rendimiento:

- El overhead principal es la copia GPU->CPU
- Para mejor rendimiento, considerar integración con NVENC/AMF en el futuro
- Timeout de 200ms para evitar bloqueos
- Buffer de último frame para mantener fluidez

### Compatibilidad:

La implementación mantiene compatibilidad total con el API existente de Weylus:
- Misma interfaz `Capturable` y `Recorder`
- Mismo formato `PixelProvider::BGR0S`
- Sin cambios en el resto del código

### Testing:

Para probar que la implementación funciona:

1. Compilar Weylus con los cambios
2. Verificar logs que mencionan "Windows Graphics Capture session started"
3. Si aparece "Failed to initialize Windows Graphics Capture", usa fallback a captrs
4. Funcionalidad idéntica desde perspectiva del usuario
