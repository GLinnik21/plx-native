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


REQUEST = r'(?:LoopReq|ContentReq|HomeReq|LibraryReq|SearchReq|PlayerReq|ItemMenuReq|WatchlistReq|PreferenceRequest|SessionCmd|AppFx)'


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


def check_screens(root):
    tests = rust.wholly_test_files(root)
    return [f'{path}: {error}' for path in sorted(root.rglob('*.rs'))
            if path.resolve() not in tests
            for error in screen_violations(path.read_text(encoding='utf-8'))]


if __name__ == '__main__':
    import sys
    try:
        errors = check_screens(Path('rust-modules/screens/src'))
        helper = Path('rust-modules/appkit/src/player_hud/state.rs')
        errors.extend(f'{helper}: {error}' for error in scrub_helper_violations(helper.read_text(encoding='utf-8')))
        for path in (helper, Path('rust-modules/appkit/src/player_hud/classic.rs')):
            errors.extend(f'{path}: {error}' for error in presentation_helper_violations(path.read_text(encoding='utf-8')))
    except (OSError, ValueError, KeyError) as error:
        errors = [str(error)]
    for error in errors:
        print('screen-runtime-io: ' + error, file=sys.stderr)
    sys.exit(bool(errors))
