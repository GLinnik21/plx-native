"""Grade finite poster workloads from actual renderer/cache counters, offline."""
import re

FIELDS = ('ms frames draws ready moving moving_frames moving_ms unknown requested '
          'requested_moving refused_new refused_evicted refused_retry rearmed uploads lost '
          'last_draws last_ready complete snap_begin_milli snap_end_milli '
          'shelf_start_px shelf_end_px shelf_span_px shelf_v_milli').split()
PHASES = {'settle': ('warm', 'move', 'settle'),
          'eviction': ('warm', 'seed1', 'seed2', 'reverse', 'settle'),
          'dive': ('warm', 'hero', 'dive', 'settle')}


def grade(spec, lines):
    kind = spec['kind']
    phases = {}
    done = False
    order = []
    for line in lines:
        if 'poster-gate:' not in line:
            continue
        values = dict(re.findall(r'(\w+)=([\w.-]+)', line.split('poster-gate:', 1)[1]))
        if values.get('kind') != kind:
            continue
        phase = values.get('phase')
        if phase == 'failed':
            return False, 'poster gate scene reported failure'
        if phase == 'done':
            done = True
            continue
        if phase == 'armed':
            continue
        if phase not in PHASES[kind] or phase in phases:
            return False, f'unknown or repeated poster phase {phase}'
        try:
            numbers = {key: int(values[key]) for key in FIELDS}
        except (KeyError, ValueError):
            return False, f'incomplete poster telemetry for {phase}'
        if min(numbers.values()) < 0 or numbers['complete'] != 1:
            return False, f'incomplete poster workload {phase}'
        if numbers['requested_moving']:
            return False, f'{phase} admitted {numbers["requested_moving"]} source requests while cards moved fast'
        phases[phase] = numbers
        order.append(phase)
    if not done or tuple(order) != PHASES[kind]:
        return False, f'poster workload did not finish all {kind} phases'
    moving = phases[{'settle': 'move', 'eviction': 'reverse', 'dive': 'dive'}[kind]]
    if moving['moving_frames'] < 10 or moving['moving_ms'] <= 0 or moving['moving'] <= 0:
        return False, 'too few genuinely moving card frames'
    fps = (moving['moving_frames'] - 1) * 1000.0 / moving['moving_ms']
    if fps < spec.get('moving_fps_floor', 55):
        return False, f'moving cards presented at {fps:.1f} fps'
    if sum(moving[k] for k in ('refused_new', 'refused_evicted', 'refused_retry')) <= 0:
        return False, 'motion phase never exercised a refused poster request'
    settled = phases['settle']
    if (settled['last_draws'] < 6 or settled['last_ready'] != settled['last_draws']
            or settled['ready'] <= 0 or settled['requested'] <= 0 or settled['uploads'] <= 0):
        return False, 'settling did not request, upload and render the complete visible art window'
    if kind == 'eviction':
        if sum(phases[p]['lost'] for p in ('warm', 'seed1', 'seed2')) <= 0:
            return False, 'seed windows never evicted a real texture'
        if moving['refused_evicted'] <= 0 or settled['rearmed'] <= 0:
            return False, 'reversal never refused then rearmed an evicted source slot'
    if kind == 'dive':
        offset = phases['warm']['shelf_end_px']
        if offset < 1000:
            return False, 'dive never established a substantial retained shelf offset'
        for phase in ('hero', 'dive', 'settle'):
            p = phases[phase]
            if (abs(p['shelf_start_px'] - offset) > 1 or abs(p['shelf_end_px'] - offset) > 1
                    or p['shelf_span_px'] > 1 or p['shelf_v_milli'] > 1000):
                return False, f'{phase} changed the shelf spring instead of retaining its offset'
        if (phases['hero']['snap_end_milli'] > 10 or phases['dive']['snap_begin_milli'] > 10
                or phases['dive']['snap_end_milli'] < 900 or phases['settle']['snap_end_milli'] < 990):
            return False, 'the measured dive did not travel from hero to grid'
    return True, (f'poster {kind}: {fps:.1f} moving fps; '
                  f'settled art {settled["last_ready"]}/{settled["last_draws"]}; '
                  f'{settled["uploads"]} uploads after motion, zero moving admissions')
