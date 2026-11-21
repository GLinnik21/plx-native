#!/usr/bin/env python3
"""Production-token boundary checks, not a whole-program IO/call-graph proof.

Screen helpers are scanned regardless of reachability/name. External IO ports are
not scanned as screens; transitive calls into other crates still require review.
All possibly-production feature combinations are included, never just defaults.
"""
from pathlib import Path
import re
import rust_test_modules as rust


def production_tokens(source):
    tokens = rust.lex(source)
    ends = rust.group_ends(tokens)
    result, i = [], 0
    while i < len(tokens):
        if tokens[i:i + 2] == [('code', '#'), ('code', '[')]:
            end = ends[i + 1]
            attr = tokens[i + 2:end]
            if attr[:2] == [('code', 'cfg'), ('code', '(')] and rust.cfg_without_tests(attr[2:-1]) == {False}:
                i = end + 1
                # Skip intervening attributes, then the complete item/statement.
                while tokens[i:i + 2] == [('code', '#'), ('code', '[')]:
                    i = ends[i + 1] + 1
                while i < len(tokens):
                    if tokens[i] == ('code', '{'):
                        i = ends[i] + 1
                        break
                    if tokens[i][0] == 'code' and tokens[i][1] in (';', ','):
                        i += 1
                        break
                    i = ends[i] + 1 if i in ends else i + 1
                continue
            # Attributes are not runtime expressions (but are retained by cfg evaluation above).
            i = end + 1
            continue
        result.append(tokens[i])
        i += 1
    return result


def code(tokens):
    values = [value if kind == 'code' else 'STRING' for kind, value in tokens]
    return ''.join((' ' if i and values[i - 1][-1:].isalnum() and value[:1].isalnum() else '') + value
                   for i, value in enumerate(values))


# Known ambient machine inputs, not every media API: pure ladders, IO workers and
# GL ports stay outside this guard. Imports/function values count as reads too.
AMBIENT_PREFERENCES = (
    'quality', 'direct_play_mode', 'subtitle_tone', 'subtitle_size', 'subtitle_position',
    'hdr_overlay_brightness', 'next_episode_mode', 'deck_press', 'skip_interval',
    'instant_dpad_seeking', 'uri_engine', 'dolby_vision_mode',
)
PROFILE_SETTINGS = (
    'QualityPref', 'DirectPlayModePref', 'SubtitleTonePref', 'SubtitleSizePref',
    'SubtitlePositionPref', 'HdrOverlayBrightnessPref', 'NextEpisodeModePref',
    'DeckPressPref', 'SkipIntervalPref', 'InstantDpadSeekingPref', 'UriEnginePref',
    'DolbyVisionModePref',
)


def preference_violations(text):
    errors = []
    names = '(?:' + '|'.join(AMBIENT_PREFERENCES) + ')'
    modules = {'route', 'player', 'prefs'}
    # Track direct module aliases so `use plx_media::route as playback` cannot hide a call.
    imports = re.findall(r'\buse\s+([^;]+);', text)
    for item in imports:
        modules.update(re.findall(r'\b(?:route|player|prefs)\s+as\s+(\w+)', item))
        if re.search(r'\b(?:plx_media|route|player|prefs)\b', item) and re.search(r'\b' + names + r'\b', item):
            errors.append('screen ambient preference import must be a captured typed input')
    if re.search(r'\b(?:' + '|'.join(sorted(modules)) + r')::' + names + r'\b', text):
        errors.append('screen ambient preference read must be a captured typed input')
    if re.search(r'\b(?:' + '|'.join(PROFILE_SETTINGS) + r')::live\b', text):
        errors.append('screen live preference cell must be a captured typed input')
    # The shared helper is now typed. Reexports with a typed argument are valid;
    # zero-argument calls through its known aliases are the historical ambient API.
    scrub_names = {'scrub_step_ns'}
    for item in imports:
        scrub_names.update(re.findall(r'\bscrub_step_ns\s+as\s+(\w+)', item))
    scrub_names.update(re.findall(r'\blet\s+(\w+)\s*=\s*[\w:]*scrub_step_ns\s*;', text))
    if re.search(r'\b(?:' + '|'.join(sorted(scrub_names)) + r')\(\)', text):
        errors.append('scrub step requires a captured typed SkipInterval')
    return errors


def scrub_helper_violations(source):
    # Check the actual intercrate helper body, not the whole appkit IO/render port.
    tokens = production_tokens(source)
    ends = rust.group_ends(tokens)
    errors = []
    text = code(tokens)
    module_aliases = re.findall(r'\b(?:route|player|prefs)\s+as\s+\w+', text)
    alias_context = ''.join('use plx_media::' + item + ';' for item in module_aliases)
    for i, token in enumerate(tokens):
        if token == ('code', 'fn') and tokens[i + 1:i + 2] == [('code', 'scrub_step_ns')]:
            j = i + 2
            while j < len(tokens) and tokens[j] != ('code', '{'):
                j += 1
            if j < len(tokens):
                errors.extend(preference_violations(alias_context + code(tokens[i:ends[j] + 1])))
    # Imports in the helper's module can also conceal the ambient getter.
    for item in re.findall(r'\buse\s+([^;]+);', code(tokens)):
        if re.search(r'\bskip_interval\b', item):
            errors.append('shared scrub helper imports ambient skip interval')
    return errors


def presentation_helper_violations(source):
    """Follow the known shared presentation roots and same-module callees, not IO ports."""
    tokens = production_tokens(source)
    ends = rust.group_ends(tokens)
    bodies = {}
    for i, token in enumerate(tokens[:-1]):
        if token == ('code', 'fn'):
            name = tokens[i + 1][1]
            j = i + 2
            while j < len(tokens) and tokens[j] not in [('code', '{'), ('code', ';')]:
                j += 1
            if j < len(tokens) and tokens[j] == ('code', '{'):
                bodies[name] = code(tokens[j + 1:ends[j]])
    imports = ';'.join(re.findall(r'\buse\s+[^;]+;', code(tokens)))
    pending = ['busy', 'transport_hidden', 'busy_surface', 'busy_surface_with', 'captured', 'draw_readout', 'draw_failed_readout']
    seen, errors = set(), []
    forbidden = r'\b(?:devtrig|error_now|error_captured|error_caption|failure_context|failure_context_captured|audio_switching)\b|\bplayer::state\b'
    player_modules = {'player'} | set(re.findall(r'\bplayer\s+as\s+(\w+)', imports))
    while pending:
        name = pending.pop()
        if name in seen or name not in bodies:
            continue
        seen.add(name)
        text = bodies[name]
        errors.extend(screen_violations(text))
        if re.search(forbidden, text + imports) or any(re.search(r'\b' + re.escape(module) + r'::state\b', text) for module in player_modules):
            errors.append('shared player presentation helper must consume complete captured facts')
        pending.extend(callee for callee in bodies if re.search(r'\b' + re.escape(callee) + r'\(', text))
    return errors


def root_settings_violations(source):
    tokens = production_tokens(source)
    ends = rust.group_ends(tokens)
    text = code(tokens)
    imports = ';'.join(re.findall(r'\buse\s+[^;]+;', text))
    errors = []
    for i, token in enumerate(tokens[:-1]):
        if token == ('code', 'impl') and tokens[i + 1][1] in ('RootPage', 'LanguagePage'):
            j = i + 2
            while j < len(tokens) and tokens[j] != ('code', '{'):
                j += 1
            body = code(tokens[j + 1:ends[j]])
            ambient_import = re.search(r'\b(?:route|prefs)\b', imports) and re.search(r'\bunder_posters\b', imports)
            if ambient_import or re.search(r'\bunder_posters\s*\(\s*\)|\b(?:saved_preference|running_preference)\b|\bcurrent\(\)\.preference\(', body + imports):
                errors.append('Settings root/language decisions require owned install and profile facts')
    return errors


def screen_violations(source):
    text = code(production_tokens(source))
    errors = []
    if re.search(r'\bdevtrig\b', text):
        errors.append('screen runtime devtrig access/import must be a captured input')
    # Known transitive failure resolvers sample player globals/diagnostics even when
    # handed a Session. Imports and function values must not hide them in step helpers.
    if re.search(r'\b(?:error_now|error_caption|error_captured|failure_context|failure_context_captured)\b', text):
        errors.append('screen failure decisions require the complete captured presentation')
    # Imports, aliases and function values count too, so a helper cannot hide the read.
    if re.search(r'\b(?:std|alloc)::(?:fs|\{[^;]*\bfs\b)|\bfs::|\b(?:File|OpenOptions)::|\.(?:read_to_string|read_to_end)\(', text):
        errors.append('screen filesystem access/import must leave as an effect')
    return errors + preference_violations(text) + root_settings_violations(source)


REQUEST = r'(?:LoopReq|ContentReq|HomeReq|LibraryReq|SearchReq|PlayerReq|ItemMenuReq|WatchlistReq|PreferenceRequest|SessionCmd|AppFx|InlineAppFx|ExternalAppFx|ResourceAppFx)'


def command_storage_violations(source):
    tokens = production_tokens(source)
    ends = rust.group_ends(tokens)
    storage = []
    for i, token in enumerate(tokens):
        if token == ('code', 'struct'):
            j = i + 1
            while j < len(tokens) and tokens[j][1] not in ('{', ';'):
                j += 1
            if j < len(tokens) and tokens[j][1] == '{':
                storage.append(code(tokens[j + 1:ends[j]]))
    text = code(tokens)
    # Typed local/static storage also counts; a function's transient Vec parameter
    # (player_requests consumes one taken command) is not retained family storage.
    storage.extend(re.findall(r'\b(?:let|static)\s+[^;{}]*', text))
    if any(re.search(r'\b(?:Vec|VecDeque|SmallVec)\s*<[^;{}>]*\b' + REQUEST + r'\b', part) for part in storage):
        return ['family command accumulator outside dispatcher FIFO']
    return []


def function_body(source, name, production=True):
    tokens = production_tokens(source) if production else rust.lex(source)
    ends = rust.group_ends(tokens)
    bodies = []
    for i, token in enumerate(tokens):
        if token == ('code', 'fn') and tokens[i + 1:i + 2] == [('code', name)]:
            j = i + 2
            while j < len(tokens) and tokens[j][1] not in ('{', ';'):
                j += 1
            if j < len(tokens) and tokens[j][1] == '{':
                bodies.append(code(tokens[j + 1:ends[j]]))
    return bodies[0] if len(bodies) == 1 else ''


def product_frame_violations(run_source, dispatch_source):
    """Token-level ownership/order pins; Rust tests prove the AppHost behavior."""
    errors = []
    outer = function_body(run_source, 'run')
    owner = function_body(dispatch_source, 'product_frame')
    if outer.count('Dispatcher::product_frame(') != 1 or re.search(
            r'\bruntime\.(?:pump|ingest|results|step|publish|admit|prepare|draw_and_swap|skipped|end)\b', outer) or any(
            re.search(r'\b' + name + r'\b', outer) for name in
            ('ingest', 'update', 'prepare_window', 'present_and_swap', 'begin_product_turn')):
        errors.append('product loop must call one dispatcher protocol, not native phases')
    callbacks = ('pump', 'ingest', 'results', 'step', 'publish', 'admit', 'prepare', 'draw_and_swap')
    positions = [owner.find('runtime.' + name + '(') for name in callbacks]
    if any(owner.count('runtime.' + name + '(') != 1 for name in callbacks) or positions != sorted(positions):
        errors.append('product callback order/count changed')
    if ('if runtime.admit(demand){' not in owner
            or 'let swapped=runtime.draw_and_swap();runtime.dispatcher().finish_presentation(swapped);if swapped{runtime.dispatcher().present.acknowledge(now);}' not in owner):
        errors.append('native admission and actual swap must guard GPU work and acknowledgement')
    admitted = owner.split('if runtime.admit(demand){', 1)[-1].split('}else{', 1)[0]
    if 'runtime.prepare();' not in admitted or 'runtime.draw_and_swap()' not in admitted:
        errors.append('GPU work escaped admission')
    if owner.count('present.take(') != 1 or owner.count('present.acknowledge(') != 1:
        errors.append('one admission and actual-swap acknowledgement required')
    if 'present.acknowledge(' in code(production_tokens(run_source)):
        errors.append('native resource port must not acknowledge the dispatcher gate')
    return errors


def effect_partition_violations(bridge_source, dispatch_source):
    """Backstop for the compiler-checked disjoint execution contracts."""
    errors = []
    bridge = code(production_tokens(bridge_source))
    dispatch = code(production_tokens(dispatch_source))
    if 'external_app_fx' in bridge + dispatch:
        errors.append('boolean app-effect classifier bypasses the owned type partition')
    if 'route_app_fx' in dispatch:
        errors.append('Rig must not override the consuming application effect route')
    drain = function_body(dispatch_source, 'drain')
    if drain.count('plx_machine::machine::AppEffect::route(app_fx)') != 1:
        errors.append('dispatcher must consume the associated application effect route directly once')
    if 'fx:InlineAppFx' not in bridge or 'ExternalFx<H>' not in dispatch:
        errors.append('inline executor and yielded envelope must retain disjoint effect types')
    for name in ('app_effect', 'execute_yielded_command'):
        body = function_body(bridge_source, name)
        if not body or 'unreachable!' in body:
            errors.append('effect executor must exhaust its admitted type without unreachable arms')
    raw = function_body(bridge_source, 'execute_yielded_command', production=False)
    if '#[cfg(test)]' in raw:
        errors.append('passive command observations belong behind the observer boundary')
    if 'Observation::begin(' not in raw or 'observation.finish(' not in raw:
        errors.append('one passive command observation boundary must bracket execution')
    return errors


def owned_ui_state_violations(source, family):
    """Bounded producer-root storage guard, not a transitive or whole-program proof.

    Every production static in the reviewed hint/modal/navigation roots needs an exact render
    name AND type. A cache-looking name alone never exempts logical publication.
    Instance fields and cfg(test) fixture storage remain ordinary owned state.
    """
    tokens = production_tokens(source)
    ends = rust.group_ends(tokens)
    text = code(tokens)
    errors = []

    def compact_type(value):
        return re.sub(r'\b(?:std::cell::|std::sync::atomic::|crate::containers::modal::|plx_base::surface::)', '', value).replace(' ', '')

    render = {} if family == 'navigation' else {'LAYOUT': 'RefCell<Option<(usize,f32,std::rc::Rc<Layout>)>>'} if family == 'hint' else {
        'TRANSITION_OWNS': 'AtomicBool',
        'CACHE': 'RefCell<FrameCache>', 'HELD': 'Cell<Held>',
        'TARGET': 'RefCell<Option<PageTarget>>',
        'CACHE_OWNER': 'RefCell<Option<ModalCacheKey>>',
        'GROUND_DRAWN': 'AtomicBool', 'CAPTURE_OWED': 'AtomicBool',
        'CAPTURE_POINTLESS': 'AtomicBool', 'PAGE_EPOCH': 'AtomicU32',
        'GROUND_DEFERRED': 'AtomicBool', 'REFLOW': 'AtomicBool',
        'REFLOW_EXIT': 'AtomicU8', 'FADE_ON': 'AtomicBool', 'FADE_B': 'AtomicU32',
    }
    for i, token in enumerate(tokens):
        if token != ('code', 'static'):
            continue
        j = i + 1
        if tokens[j:j + 1] == [('code', 'mut')]:
            errors.append(f'{family} mutable static cannot own logical state')
            continue
        # Ignore a lifetime such as &'static str: declarations have name + colon.
        if j + 1 >= len(tokens) or tokens[j + 1] != ('code', ':'):
            continue
        name = tokens[j][1]
        k = j + 2
        while k < len(tokens) and tokens[k][1] not in ('=', ';', '{'):
            k = ends[k] + 1 if k in ends else k + 1
        shape = compact_type(code(tokens[j + 2:k]))
        if render.get(name) != shape:
            errors.append(f'{family} ambient storage {name} must be owned, not a logical TLS/static facade')

    if family == 'navigation':
        for name in ('page_image_held', 'set_page_image_held'):
            if re.search(r'\bfn\s+' + name + r'\b', text):
                errors.append('page-image policy requires the owning immutable context, not a publisher/query facade')

    if family == 'modal':
        # Identity/signature is safe only as a GPU cache key, never as a readout.
        for i, token in enumerate(tokens[:-2]):
            if token == ('code', 'struct') and tokens[i + 1] == ('code', 'ModalCacheKey'):
                j = i + 2
                if tokens[j] != ('code', '{'):
                    errors.append('modal cache identity must retain its reviewed field shape')
                    continue
                fields = re.sub(r'\bstd::sync::', '', code(tokens[j + 1:ends[j]])).replace(' ', '').rstrip(',')
                if fields != 'owner:Arc<()>,layers:Vec<(EntryId,bool)>':
                    errors.append('modal cache identity cannot retain logical readout/policy facts')
        for name in ('page_veiled', 'page_veil', 'is_open', 'host_users', 'host_closing', 'shown_this_run'):
            if re.search(r'\bfn\s+' + name + r'\(\)', text):
                errors.append('modal logical query requires an explicit owning readout')
        for i, token in enumerate(tokens[:-1]):
            if token != ('code', 'fn'):
                continue
            name = tokens[i + 1][1]
            j = i + 2
            while j < len(tokens) and tokens[j][1] not in ('{', ';'):
                j = ends[j] + 1 if j in ends else j + 1
            if j < len(tokens) and tokens[j][1] == '{' and name != 'bind_owner':
                if '.same_snapshot(' in code(tokens[j + 1:ends[j]]):
                    errors.append('modal snapshot identity is restricted to render cache invalidation')
    return errors


def player_draw_violations(source):
    """Follow actual self-helper calls: paint may change caches, not Watch/timer policy."""
    tokens = production_tokens(source)
    ends = rust.group_ends(tokens)
    bodies = {}
    for i, token in enumerate(tokens):
        if token == ('code', 'fn') and i + 1 < len(tokens):
            j = i + 2
            while j < len(tokens) and tokens[j][1] not in ('{', ';'):
                j += 1
            if j < len(tokens) and tokens[j][1] == '{':
                bodies[tokens[i + 1][1]] = code(tokens[j + 1:ends[j]])
    pending, seen, errors = ['draw', 'draw_hud'], set(), []
    while pending:
        name = pending.pop()
        if name in seen or name not in bodies:
            continue
        seen.add(name)
        body = bodies[name]
        if re.search(r'\bself\.drawn_watch(?:=|\.(?:set|take|replace)\()', body) or re.search(
                r'\bself\.auto_skip\.(?:presented|tick|cancel|reset|resolve)\(', body):
            errors.append('player draw/helper mutates canonical Watch/timer state: ' + name)
        pending.extend(re.findall(r'\bself\.(\w+)\(', body))
    if 'ScreenEvent::App(AppMsg::PlayerPresented' in code(tokens):
        errors.append('ordinary App input cannot grant player presentation authority')
    write = bodies.get('write', '')
    if 'self.auto_skip.snapshot()' not in write or 'self.drawn_watch.as_ref()' not in write:
        errors.append('player Watch/timer facts must remain in LogicalState')
    return errors


def observer_violations(dispatch_source, bridge_source):
    """Bound the actual passive fanout and single decision port; runtime tests grade callbacks."""
    text = code(production_tokens(dispatch_source))
    errors = []
    tokens = production_tokens(dispatch_source)
    ends = rust.group_ends(tokens)
    tap = ''
    for i, token in enumerate(tokens):
        if token == ('code', 'trait') and tokens[i + 1] == ('code', 'Tap'):
            opening = next(j for j in range(i + 2, len(tokens)) if tokens[j] == ('code', '{'))
            tap = code(tokens[opening + 1:ends[opening]])
            break
    decisions = ('resolution_mode', 'resolution_active', 'resolution_error', 'resolve_focus', 'resolve_hit', 'resolve_id',
                 'focus_continuation')
    if not tap or any(re.search(r'\bfn ' + name + r'\b', tap) for name in decisions):
        errors.append('Tap must contain passive callbacks only, never replay decisions')
    fanout = function_body(dispatch_source, 'frame_tap')
    if 'PassiveObservers{owned,caller}' not in fanout or 'FrameObservers::Owned(tap)=>Some(tap)' not in fanout:
        errors.append('owned observer must retain the explicit passive caller')
    authority = function_body(dispatch_source, 'frame_authority')
    if (authority.count('call(replay)') != 1 or 'FrameObservers::Replay(replay)=>call(replay)' not in authority
            or 'FrameObservers::Caller|FrameObservers::Owned(_)=>call(&mut LiveAuthority)' not in authority
            or 'caller' in authority):
        errors.append('replay decisions require the one explicit owned Replay capability')
    if text.count('owned.$name($($arg),*)') != 1 or text.count('self.caller.$name($($arg),*)') != 1:
        errors.append('passive callback forwarding must reach each observer exactly once')
    callbacks = ('tick', 'input', 'result', 'presentation', 'effect', 'present', 'focus', 'frame_done', 'command_done')
    forwarding = text.split('observe_both!{', 1)[-1].split('}', 1)[0]
    if any(len(re.findall(r'\b' + name + r'\(', forwarding)) != 1 for name in callbacks):
        errors.append('every passive callback must participate once in fanout')
    if 'let mut once=||*cached.get_or_insert_with(&mut*hash)' not in text:
        errors.append('passive state fanout must memoize the lazy logical-state hash')
    if 'FrameObservers::Replay(self.resources.rec)' not in code(production_tokens(bridge_source)):
        errors.append('actual CommandRig must explicitly own the recorder replay capability')
    return errors


def check_owned_ui_state(root):
    paths = (
        ('hint', root / 'rust-modules/ui/src/hold_hint.rs'),
        ('modal', root / 'rust-modules/ui/src/containers/modal.rs'),
        ('modal', root / 'rust-modules/ui/src/popover.rs'),
        ('navigation', root / 'rust-modules/ui/src/containers/transition.rs'),
        ('navigation', root / 'rust-modules/ui/src/dispatch.rs'),
        ('navigation', root / 'rust-modules/ui/src/cards/landing.rs'),
        ('navigation', root / 'rust-modules/ui/src/cards/shelf.rs'),
        ('navigation', root / 'rust-modules/ui/src/cards/grid.rs'),
    )
    return [f'{path}: {error}' for family, path in paths
            for error in owned_ui_state_violations(path.read_text(encoding='utf-8'), family)]


def external_arg_match_violations(source):
    """Reject closed AppArg ladders outside registration, including renamed helpers.

    Partial resource queries remain legal. Group-aware arm heads distinguish a catch-all
    `_` from payload patterns such as AppArg::Settings(_); nested matches cannot hide it.
    This is a bounded source guard, supplemented by the actual fresh-variant compiler proof.
    """
    tokens = production_tokens(source)
    ends = rust.group_ends(tokens)
    text = code(tokens)
    names = {'AppArg'} | set(re.findall(r'\bAppArg\s+as\s+(\w+)', text))
    errors = []
    for at, token in enumerate(tokens):
        if token != ('code', 'match'):
            continue
        opening = at + 1
        while opening < len(tokens) and tokens[opening] != ('code', '{'):
            opening = ends[opening] + 1 if opening in ends else opening + 1
        if opening == len(tokens):
            continue
        closing, pos, heads = ends[opening], opening + 1, []
        while pos < closing:
            start = pos
            while pos < closing and tokens[pos:pos + 2] != [('code', '='), ('code', '>')]:
                pos = ends[pos] + 1 if pos in ends else pos + 1
            if pos == closing:
                break
            heads.append(code(tokens[start:pos]))
            pos += 2
            while pos < closing and tokens[pos] != ('code', ','):
                if tokens[pos] == ('code', '{'):
                    pos = ends[pos] + 1
                    break  # block-bodied arms need no comma
                pos = ends[pos] + 1 if pos in ends else pos + 1
            if pos < closing and tokens[pos] == ('code', ','):
                pos += 1
        has_arg = any(any(re.match(r'(?:\w+::)*' + re.escape(name) + r'::', head) for name in names) for head in heads)
        catch_all = any(re.fullmatch(r'(?:ref |mut )?\w+', head) for head in heads)
        if has_arg and not catch_all:
            errors.append('closed AppArg match outside registry requires ordinary screen edits')
    return errors


def page_census_violations(source):
    if function_body(source, 'every_route', production=False) != 'plx_screens::registry::every_page_arg()':
        return ['page census must delegate to allowed registry, not an external array/helper']
    return []


# The application crate and the two platform crates that took files out of it (`port.rs`, `system.rs`,
# `platform_desktop.rs` and `bin/sim.rs` were `rust-modules/src`'s): the ladder gate read all of them as one tree.
EXTENSION_ROOTS = ('rust-modules/src', 'rust-modules/platform-webos/src', 'rust-modules/platform-desktop/src')


def check_extension_registration(root):
    source_root = root / EXTENSION_ROOTS[0]
    errors = []
    for rel in EXTENSION_ROOTS:
        base = root / rel
        if not base.is_dir():
            # A missing directory would make `rglob` yield nothing, which reads as "no ladder".
            errors.append(f'{rel}: a directory this gate reads is missing (moved? fix EXTENSION_ROOTS in ci/ui_boundaries.py)')
            continue
        tests = rust.wholly_test_files(base)
        errors.extend(f'{path}: {error}' for path in sorted(base.rglob('*.rs'))
                      if path.resolve() not in tests
                      for error in external_arg_match_violations(path.read_text(encoding='utf-8')))
    words = source_root / 'app/words.rs'
    if words.exists():
        errors.extend(f'{words}: {error}' for error in page_census_violations(words.read_text(encoding='utf-8')))
    return errors


def check_screens(root):
    tests = rust.wholly_test_files(root)
    return [f'{path}: {error}' for path in sorted(root.rglob('*.rs'))
            if path.resolve() not in tests
            for error in screen_violations(path.read_text(encoding='utf-8'))]


if __name__ == '__main__':
    import sys
    try:
        errors = check_screens(Path('rust-modules/screens/src'))
        errors.extend(check_owned_ui_state(Path('.')))
        errors.extend(check_extension_registration(Path('.')))
        errors.extend(player_draw_violations(Path('rust-modules/screens/src/player/mod.rs').read_text(encoding='utf-8')))
        errors.extend(product_frame_violations(
            Path('rust-modules/src/app/run.rs').read_text(encoding='utf-8'),
            Path('rust-modules/ui/src/dispatch.rs').read_text(encoding='utf-8')))
        errors.extend(effect_partition_violations(
            Path('rust-modules/src/app/bridge.rs').read_text(encoding='utf-8'),
            Path('rust-modules/ui/src/dispatch.rs').read_text(encoding='utf-8')))
        errors.extend(observer_violations(
            Path('rust-modules/ui/src/dispatch.rs').read_text(encoding='utf-8'),
            Path('rust-modules/src/app/bridge.rs').read_text(encoding='utf-8')))
        helper = Path('rust-modules/appkit/src/player_hud/state.rs')
        errors.extend(f'{helper}: {error}' for error in scrub_helper_violations(helper.read_text(encoding='utf-8')))
        for path in (helper, Path('rust-modules/appkit/src/player_hud/classic.rs')):
            errors.extend(f'{path}: {error}' for error in presentation_helper_violations(path.read_text(encoding='utf-8')))
    except (OSError, ValueError, KeyError) as error:
        errors = [str(error)]
    for error in errors:
        print('screen-runtime-io: ' + error, file=sys.stderr)
    sys.exit(bool(errors))
