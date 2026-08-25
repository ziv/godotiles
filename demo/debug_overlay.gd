extends Node3D
## Debug overlay: tile bounds as wire boxes (L) and zoom labels above tile centers (K),
## green = desired, red = resident but no longer desired. Reads
## `TerrainStreamer.get_resident_tiles()` every frame while enabled.

const BOX_HEIGHT := 1000.0

var show_bounds := false
var show_labels := false

var _mesh := ImmediateMesh.new()
var _mesh_instance := MeshInstance3D.new()
var _labels: Array[Label] = []
var _label_layer := CanvasLayer.new()


func _ready() -> void:
	var mat := StandardMaterial3D.new()
	mat.shading_mode = BaseMaterial3D.SHADING_MODE_UNSHADED
	mat.albedo_color = Color.GREEN
	mat.no_depth_test = false
	_mesh_instance.mesh = _mesh
	_mesh_instance.material_override = mat
	_mesh_instance.custom_aabb = AABB(Vector3(-1e7, -1e5, -1e7), Vector3(2e7, 2e5, 2e7))
	add_child(_mesh_instance)
	add_child(_label_layer)


func _process(_dt: float) -> void:
	var terrain := get_parent().get_node_or_null("TerrainStreamer") as TerrainStreamer
	var camera := get_viewport().get_camera_3d()
	_mesh.clear_surfaces()
	if terrain == null or camera == null or not (show_bounds or show_labels):
		_hide_labels(0)
		return
	var tiles := terrain.get_resident_tiles()
	if show_bounds:
		_mesh.surface_begin(Mesh.PRIMITIVE_LINES)
		for t in tiles:
			if not t.visible:
				continue
			_box(t.center, t.size)
		_mesh.surface_end()
	if show_labels:
		var n := 0
		for t in tiles:
			if not t.visible or camera.is_position_behind(t.center):
				continue
			var l := _label(n)
			l.text = str(t.zoom)
			l.add_theme_color_override("font_color", Color.GREEN if t.desired else Color.RED)
			l.position = camera.unproject_position(t.center)
			l.visible = true
			n += 1
		_hide_labels(n)
	else:
		_hide_labels(0)


func _box(center: Vector3, size: float) -> void:
	var h := size / 2.0
	var y0 := 0.0
	var y1 := BOX_HEIGHT
	var c := [
		center + Vector3(-h, y0, -h), center + Vector3(h, y0, -h),
		center + Vector3(h, y0, h), center + Vector3(-h, y0, h),
		center + Vector3(-h, y1, -h), center + Vector3(h, y1, -h),
		center + Vector3(h, y1, h), center + Vector3(-h, y1, h),
	]
	var edges := [[0, 1], [1, 2], [2, 3], [3, 0], [4, 5], [5, 6], [6, 7], [7, 4], [0, 4], [1, 5], [2, 6], [3, 7]]
	for e in edges:
		_mesh.surface_add_vertex(c[e[0]])
		_mesh.surface_add_vertex(c[e[1]])


func _label(i: int) -> Label:
	while _labels.size() <= i:
		var l := Label.new()
		l.add_theme_font_size_override("font_size", 15)
		_label_layer.add_child(l)
		_labels.append(l)
	return _labels[i]


func _hide_labels(from: int) -> void:
	for i in range(from, _labels.size()):
		_labels[i].visible = false
