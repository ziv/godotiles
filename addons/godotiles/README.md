# godotiles addon

3D geospatial terrain streaming for Godot 4 — streams satellite imagery and elevation tiles
around a camera and renders them as GPU-displaced terrain. Full documentation, demo project and
sources: <https://github.com/ziv/godotiles>.

## Install

Copy this folder to `addons/godotiles/` in your project. The `.gdextension` file is discovered
automatically; the classes `TerrainStreamer`, `TerrainWorldConfig`, `TerrainStreamingConfig`,
`TerrainRenderingConfig` and `TerrainNetworkConfig` become available in GDScript and C#.

Prebuilt binaries live in `bin/` (macOS universal, Linux x86_64, Windows x86_64). On macOS you
may need to clear the quarantine flag of an unsigned download:
`xattr -dr com.apple.quarantine addons/godotiles/bin/macos`.

## Quick start

```gdscript
extends Node3D

func _ready() -> void:
    var terrain := TerrainStreamer.new()
    terrain.world = TerrainWorldConfig.from_lat_lon(46.206889, 9.497194)  # the Dolomites
    terrain.rendering.fog_color = Color8(102, 191, 255)                    # match your sky
    terrain.camera_path = $Camera3D.get_path()
    add_child(terrain)
    $Camera3D.near = 1.0
    $Camera3D.far = 400000.0
    $Camera3D.position = terrain.initial_position(5000.0)
```

Data: imagery © Esri; elevation / normals from the Mapzen / AWS terrain tiles. Mind their terms.
