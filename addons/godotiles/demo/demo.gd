extends Node3D
## The godotiles demo: fly over real-world terrain with a loading screen, large-world
## rebasing, ground-collision crash detection and sky-matched fog.
##
## Controls: A/D roll, Q/E yaw, W/S pitch, +/- throttle, R reset after a crash,
## K toggle zoom labels, L toggle tile bounds. Change LAT/LON to fly anywhere.

## World anchor: the Grand Canyon. Other anchors from the raytiles demo:
## the Negev (30.82691969, 34.91386236), the Dolomites (46.206889, 9.497194),
## London (51.5074, 0.1278).
const LAT := 35.97391
const LON := -113.76892

## Sky / fog color (raylib's SKYBLUE, for parity with the other demos).
const SKY := Color8(102, 191, 255)
## User-space drift (meters) that triggers a large-world rebase.
const REBASE_THRESHOLD := 4096.0
const START_ALTITUDE := 5000.0

@onready var terrain: TerrainStreamer = $TerrainStreamer
@onready var camera: Camera3D = $Camera3D
@onready var overlay: Node3D = $DebugOverlay

var loading_label: Label
var hud_label: Label
var crash_label: Label
var help_label: Label


func _ready() -> void:
	# --- terrain configuration (the streamer already ran its _ready with inspector defaults;
	# mutate the sub-resources first, then assign `world` last: that setter rebuilds the world
	# and re-reads every config)
	terrain.network.threads = 8
	terrain.rendering.fog_color = SKY
	terrain.rendering.ambient_light = Color8(200, 200, 200)
	# skirts (with the 1.01 mesh overlap below) hide the cracks between LOD levels
	terrain.rendering.skirt_drop = 500.0
	var world := TerrainWorldConfig.from_lat_lon(LAT, LON)
	# opt into greater zoom: imagery fetches natively, heightmaps above z15 are synthesized,
	# normals default to flat
	world.max_zoom = 17
	var overlap := PackedFloat32Array()
	overlap.resize(14)
	overlap.fill(1.01)
	world.skirt_overlap = overlap
	terrain.world = world

	# --- camera: over the anchor, looking down-forward
	var start := terrain.initial_position(START_ALTITUDE)
	camera.position = start
	camera.look_at(start + Vector3(-1000, -300, -1000), Vector3.UP)
	camera.home = camera.transform

	_build_ui()
	terrain.initial_load_complete.connect(func() -> void: loading_label.visible = false)
	terrain.tile_failed.connect(func(zoom: int, x: int, z: int, reason: String) -> void:
		push_warning("tile %d/%d/%d failed: %s" % [zoom, x, z, reason]))


func _process(_dt: float) -> void:
	_rebase_large_world()
	_crash_check()
	_update_hud()
	if Input.is_action_just_pressed("ui_cancel"):
		get_tree().quit()


## Keep the user-space camera near the origin: when it drifts past the threshold, shift the
## camera AND the world offset by the same amount — preserving `absolute = user − offset`.
## The streamer rebakes tile transforms when `world_offset` changes. A real game shifts every
## user-space node here.
func _rebase_large_world() -> void:
	var shift := Vector3.ZERO
	if absf(camera.position.x) > REBASE_THRESHOLD:
		shift.x = -REBASE_THRESHOLD * signf(camera.position.x)
	if absf(camera.position.z) > REBASE_THRESHOLD:
		shift.z = -REBASE_THRESHOLD * signf(camera.position.z)
	if shift != Vector3.ZERO:
		camera.position += shift
		terrain.world_offset += shift


## Compare the camera altitude against the terrain height; below ground = crash. R respawns.
func _crash_check() -> void:
	if camera.crashed:
		if Input.is_key_pressed(KEY_R):
			camera.reset(terrain.world_offset)
			crash_label.visible = false
		return
	var ground = terrain.ground_height(camera.global_position)
	if ground != null and ground > camera.position.y:
		camera.crashed = true
		crash_label.visible = true


func _update_hud() -> void:
	if terrain.is_loading:
		loading_label.text = "Loading... %.1f%%" % (terrain.loading_progress * 100.0)
	var ground = terrain.ground_height(camera.global_position)
	var agl := "n/a" if ground == null else "%.0f m" % (camera.position.y - ground)
	hud_label.text = "speed %.0f m/s   AGL %s   %d fps\nuser P %.0f %.0f %.0f   offset %.0f %.0f\ntiles resident %d   desired %d   loading %d" % [
		camera.speed, agl, Engine.get_frames_per_second(),
		camera.position.x, camera.position.y, camera.position.z,
		terrain.world_offset.x, terrain.world_offset.z,
		terrain.resident_count, terrain.desired_count, terrain.loading_count,
	]


func _unhandled_key_input(event: InputEvent) -> void:
	if event is InputEventKey and event.pressed and not event.echo:
		match event.keycode:
			KEY_K:
				overlay.show_labels = not overlay.show_labels
			KEY_L:
				overlay.show_bounds = not overlay.show_bounds


func _build_ui() -> void:
	var ui := CanvasLayer.new()
	ui.name = "UI"
	add_child(ui)

	loading_label = _label(ui, 42)
	loading_label.text = "Loading... 0%"
	loading_label.set_anchors_and_offsets_preset(Control.PRESET_CENTER)

	hud_label = _label(ui, 16)
	hud_label.position = Vector2(10, 10)

	crash_label = _label(ui, 36)
	crash_label.text = "You crashed! Press R to reset."
	crash_label.set_anchors_and_offsets_preset(Control.PRESET_CENTER)
	crash_label.visible = false

	help_label = _label(ui, 13)
	help_label.text = "A/D roll · Q/E yaw · W/S pitch · +/- throttle · R reset · K labels · L bounds · Esc quit"
	help_label.set_anchors_and_offsets_preset(Control.PRESET_BOTTOM_LEFT)
	help_label.position = Vector2(10, get_viewport().get_visible_rect().size.y - 30)


func _label(parent: Node, size: int) -> Label:
	var l := Label.new()
	l.add_theme_font_size_override("font_size", size)
	l.add_theme_color_override("font_color", Color.WHITE)
	l.add_theme_color_override("font_shadow_color", Color(0, 0, 0, 0.7))
	l.add_theme_constant_override("shadow_offset_x", 1)
	l.add_theme_constant_override("shadow_offset_y", 1)
	parent.add_child(l)
	return l
