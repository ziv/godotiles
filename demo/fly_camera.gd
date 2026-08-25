extends Camera3D
## Airplane-style fly camera, always moving forward. All rotations are around the camera's
## LOCAL axes, so yawing while banked turns like an aircraft: A/D roll, Q/E yaw, W/S pitch,
## +/- throttle. Keys set a TARGET angular velocity; the actual velocity eases toward it
## exponentially (frame-rate independent), so inputs ramp in and glide out.

## How quickly the controls ease toward their target rate (1/s): higher is snappier.
const CONTROL_RESPONSE := 5.0

var speed := 120.0                 # m/s
var ang_vel := Vector3.ZERO        # x pitch, y yaw, z roll (rad/s)
var crashed := false
var home: Transform3D


func _ready() -> void:
	home = transform


func _process(dt: float) -> void:
	if crashed:
		return
	var target := Vector3.ZERO
	if Input.is_key_pressed(KEY_A):
		target.z += 1.2 # bank left
	if Input.is_key_pressed(KEY_D):
		target.z -= 1.2 # bank right
	if Input.is_key_pressed(KEY_Q):
		target.y += 0.8 # nose left
	if Input.is_key_pressed(KEY_E):
		target.y -= 0.8 # nose right
	if Input.is_key_pressed(KEY_W):
		target.x -= 0.6 # nose down
	if Input.is_key_pressed(KEY_S):
		target.x += 0.6 # nose up
	if Input.is_key_pressed(KEY_EQUAL) or Input.is_key_pressed(KEY_KP_ADD):
		speed = minf(speed * (1.0 + dt), 3000.0)
	if Input.is_key_pressed(KEY_MINUS) or Input.is_key_pressed(KEY_KP_SUBTRACT):
		speed = maxf(speed * (1.0 - dt), 20.0)

	var blend := 1.0 - exp(-CONTROL_RESPONSE * dt)
	ang_vel = ang_vel.lerp(target, blend)
	rotate_object_local(Vector3(0, 0, 1), ang_vel.z * dt)
	rotate_object_local(Vector3(0, 1, 0), ang_vel.y * dt)
	rotate_object_local(Vector3(1, 0, 0), ang_vel.x * dt)
	position += -global_transform.basis.z * speed * dt # Godot forward is -Z


## Respawn at the initial transform, shifted into the current user space.
func reset(offset: Vector3) -> void:
	transform = home.translated(offset)
	ang_vel = Vector3.ZERO
	crashed = false
