extends Node3D
## Quick start: the smallest possible godotiles scene (mirror of raytiles' quick_start.cpp).
## Attach to a Node3D that has a Camera3D child named "Camera3D".

func _ready() -> void:
	# 1. anchor position (the Dolomites)
	var lat := 46.206889
	var lon := 9.497194

	# 2. the streamer with default configuration, anchored at lat/lon
	var terrain := TerrainStreamer.new()
	terrain.world = TerrainWorldConfig.from_lat_lon(lat, lon)
	terrain.rendering.fog_color = Color8(102, 191, 255) # match the sky
	terrain.camera_path = $Camera3D.get_path()
	add_child(terrain)

	# 3. the camera: 5000 m above the anchor, looking south-east and down; make sure
	#    it sees to the horizon (the inspector caps `far` at 4000 — code does not)
	$Camera3D.near = 1.0
	$Camera3D.far = 400000.0
	$Camera3D.position = terrain.initial_position(5000.0)
	$Camera3D.look_at($Camera3D.position + Vector3(-1000, -300, -1000), Vector3.UP)


func _process(_dt: float) -> void:
	# 4. tiles cache under .cache/ next to the project; the first run downloads them
	var terrain := get_node_or_null("TerrainStreamer") as TerrainStreamer
	if terrain and terrain.is_loading:
		print("loading %.1f%%" % (terrain.loading_progress * 100.0))
