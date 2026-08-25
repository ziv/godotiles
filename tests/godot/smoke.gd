extends SceneTree
## Headless smoke test for the Godot shell:
##   godot --headless --path . -s tests/godot/smoke.gd
## Seeds a tiny offline cache (one z9 tile: flat imagery, a flat 100 m Terrarium heightmap,
## default normals), points the streamer at a dead host, and checks that the tile becomes
## resident, the loading contract completes, and ground_height reads the seeded height.

const HEIGHT_M := 100.0


func _initialize() -> void:
	_run()


func _run() -> void:
	# the root window is not inside the tree yet during _initialize
	await process_frame
	var cache := ProjectSettings.globalize_path("user://smoke-cache")
	_seed_cache(cache)

	var root_node := Node3D.new()
	root.add_child(root_node)
	var camera := Camera3D.new()
	camera.near = 1.0
	camera.far = 400000.0
	root_node.add_child(camera)
	camera.current = true

	var terrain := TerrainStreamer.new()
	terrain.world.base_zoom = 9
	terrain.world.max_zoom = 9
	terrain.streaming.radius = 2 # disc of one base tile
	terrain.network.cache_dir = cache
	terrain.network.threads = 1
	terrain.network.texture_url = "http://127.0.0.1:9/tex/:zoom:/:x:/:y:.png"
	terrain.network.heightmap_url = "http://127.0.0.1:9/hm/:zoom:/:x:/:y:.png"
	terrain.network.normals_url = "http://127.0.0.1:9/nl/:zoom:/:x:/:y:.png"
	terrain.network.connect_timeout_sec = 0.3
	terrain.network.read_timeout_sec = 0.5
	terrain.camera_path = camera.get_path()
	root_node.add_child(terrain)

	var start := terrain.initial_position(5000.0)
	camera.position = start
	camera.look_at(start + Vector3(1000, -500, 1000), Vector3.UP)

	var frames := 0
	while terrain.is_loading and frames < 600:
		await process_frame
		frames += 1

	var ok := true
	ok = _check(not terrain.is_loading, "initial load completed (%d frames)" % frames) and ok
	ok = _check(terrain.resident_count >= 1, "resident_count >= 1 (got %d)" % terrain.resident_count) and ok
	ok = _check(terrain.desired_count == 1, "desired_count == 1 (got %d)" % terrain.desired_count) and ok
	ok = _check(terrain.loading_progress == 1.0, "loading_progress == 1") and ok
	var h = terrain.ground_height(start)
	ok = _check(h != null and absf(float(h) - HEIGHT_M) < 0.51, "ground_height ≈ %.0f m (got %s)" % [HEIGHT_M, str(h)]) and ok
	ok = _check(terrain.ground_height(Vector3(-1e6, 0, -1e6)) == null, "ground_height outside coverage is null") and ok
	var tiles := terrain.get_resident_tiles()
	ok = _check(tiles.size() == terrain.resident_count, "get_resident_tiles matches resident_count") and ok
	if tiles.size() > 0:
		ok = _check(tiles[0].zoom == 9 and tiles[0].size == terrain.world.tile_size, "resident tile is the z9 anchor tile") and ok

	# rebase: tiles must follow the offset
	terrain.world_offset = Vector3(1000, 0, 0)
	camera.position += Vector3(1000, 0, 0)
	await process_frame
	var t2 := terrain.get_resident_tiles()
	ok = _check(t2.size() > 0 and absf(t2[0].center.x - (tiles[0].center.x + 1000.0)) < 0.01, "tile centers follow world_offset") and ok
	ok = _check(terrain.ground_height(start + Vector3(1000, 0, 0)) != null, "ground_height honors world_offset") and ok

	# a rebuild drops everything and reloads it
	terrain.rebuild()
	ok = _check(terrain.resident_count == 0 and terrain.is_loading, "rebuild resets residency + loading") and ok
	frames = 0
	while terrain.is_loading and frames < 600:
		await process_frame
		frames += 1
	ok = _check(not terrain.is_loading and terrain.resident_count >= 1, "reload after rebuild") and ok

	# teardown must free instances without errors
	terrain.queue_free()
	await process_frame
	await process_frame

	print("smoke: %s" % ("PASS" if ok else "FAIL"))
	quit(0 if ok else 1)


func _check(cond: bool, what: String) -> bool:
	print("  [%s] %s" % ["ok" if cond else "FAIL", what])
	return cond


func _seed_cache(cache: String) -> void:
	# default anchor tile is (306, 207) at z9
	var dirs := ["texture/9/306", "heightmap/9/306", "normals/9/306"]
	for d in dirs:
		DirAccess.make_dir_recursive_absolute(cache + "/" + d)
	var tex := Image.create(256, 256, false, Image.FORMAT_RGB8)
	tex.fill(Color8(120, 140, 90))
	tex.save_png(cache + "/texture/9/306/207.png")
	# Terrarium: h = r*256 + g + b/256 - 32768 → 100 m = (128, 100, 0)
	var hm := Image.create(256, 256, false, Image.FORMAT_RGB8)
	hm.fill(Color8(128, int(HEIGHT_M), 0))
	hm.save_png(cache + "/heightmap/9/306/207.png")
	var nl := Image.create(256, 256, false, Image.FORMAT_RGB8)
	nl.fill(Color8(128, 128, 255))
	nl.save_png(cache + "/normals/9/306/207.png")
