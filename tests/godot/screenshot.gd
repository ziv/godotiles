extends SceneTree
## Visual check helper (needs a window, not --headless):
##   godot --path . -s tests/godot/screenshot.gd -- out.png [seconds]
## Loads the demo scene, waits for the initial load (or the timeout), lets a few frames render
## and saves the viewport to `out.png`. Tiles download from the real providers (cached in .cache/).


func _initialize() -> void:
	_run()


func _run() -> void:
	await process_frame
	var args := OS.get_cmdline_user_args()
	var out := args[0] if args.size() > 0 else "screenshot.png"
	var wait := float(args[1]) if args.size() > 1 else 90.0

	var scene: PackedScene = load("res://demo/main.tscn")
	var main := scene.instantiate()
	root.add_child(main)
	var terrain: TerrainStreamer = main.get_node("TerrainStreamer")

	var t0 := Time.get_ticks_msec()
	while terrain.is_loading and Time.get_ticks_msec() - t0 < wait * 1000.0:
		await process_frame

	if args.size() > 2:
		# optional: AGL altitude (meters) and pitch (degrees down) for close-up checks, once the
		# ground under the anchor is known; then wait for the finer tiles
		var cam0: Camera3D = main.get_node("Camera3D")
		var agl := float(args[2])
		var pitch := float(args[3]) if args.size() > 3 else 20.0
		var start := terrain.initial_position(0.0) + terrain.world_offset
		var ground = terrain.ground_height(start)
		start.y = (float(ground) if ground != null else 0.0) + agl
		cam0.position = start
		cam0.look_at(start + Vector3(-1000, -1000 * tan(deg_to_rad(pitch)), -1000), Vector3.UP)
		cam0.home = cam0.transform
		cam0.speed = 5.0
		cam0.crashed = false
		main.crash_label.visible = false
		await process_frame
		await process_frame
		while (terrain.is_loading or terrain.loading_count > 0) and Time.get_ticks_msec() - t0 < wait * 1000.0:
			await process_frame
	if args.size() > 4:
		var overlay = main.get_node("DebugOverlay")
		overlay.show_bounds = true
		overlay.show_labels = true
	print("screenshot: loading=%s resident=%d desired=%d loading=%d after %.1fs" % [
		terrain.is_loading, terrain.resident_count, terrain.desired_count, terrain.loading_count,
		(Time.get_ticks_msec() - t0) / 1000.0])
	for i in 10:
		await process_frame
	var cam: Camera3D = main.get_node("Camera3D")
	var fwd := -cam.global_transform.basis.z
	print("screenshot: frustum self-check — ahead: %s (expect true), behind: %s (expect false)" % [
		terrain.debug_point_in_frustum(cam.global_position + fwd * 100.0),
		terrain.debug_point_in_frustum(cam.global_position - fwd * 100.0)])
	var tiles := terrain.get_resident_tiles()
	var vis := 0
	for t in tiles:
		if t.visible:
			vis += 1
	print("screenshot: %d / %d resident tiles visible" % [vis, tiles.size()])
	var img := root.get_viewport().get_texture().get_image()
	var err := img.save_png(out)
	print("screenshot: saved %s (%s)" % [out, error_string(err)])
	quit(0 if err == OK else 1)
