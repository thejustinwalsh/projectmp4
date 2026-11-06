#!/usr/bin/env python3
"""Developer-only reference comparison. Requires NumPy, no application Python.

Executes the actual pinned upstream search/pruning/scoring functions through AST
extraction; Numba decorators are identity wrappers for this small fixture.
Does not exercise librosa feature extraction or sample-boundary refinement.
"""
import argparse
import ast
import ctypes as ct
from dataclasses import dataclass
import hashlib
import json
from pathlib import Path
from typing import List, Optional, Tuple
import numpy as np

ROOT = Path(__file__).resolve().parents[3]

class Options(ct.Structure):
    _fields_ = [(name, ct.c_uint32) for name in (
        'abi', 'frames', 'beats', 'min_loop', 'max_loop', 'test_frames',
        'disable_pruning', 'max_candidates')] + [
        ('max_pair_checks', ct.c_uint64), ('max_score_frames', ct.c_uint64)]

class Pair(ct.Structure):
    _fields_ = [('start', ct.c_uint32), ('end', ct.c_uint32),
               ('note', ct.c_double), ('loudness', ct.c_double), ('score', ct.c_double)]

def upstream():
    path = ROOT / 'vendor/pymusiclooper/analysis.py'
    manifest = json.loads((path.parent / 'source.json').read_text())
    assert hashlib.sha256(path.read_bytes()).hexdigest() == manifest['files']['analysis.py']
    tree = ast.parse(path.read_text())
    names = {'LoopPair', '_norm', '_db_diff', '_find_candidate_pairs', '_prune_candidates',
             '_weights', '_calculate_loop_score', '_calculate_subseq_beat_similarity'}
    tree.body = [node for node in tree.body if isinstance(node, (ast.FunctionDef, ast.ClassDef)) and node.name in names]
    def njit(function=None, **kwargs):
        return function if function else lambda value: value
    namespace = dict(np=np, njit=njit, dataclass=dataclass, List=List, Optional=Optional, Tuple=Tuple)
    exec(compile(tree, str(path), 'exec'), namespace)
    return namespace

def pointer(array, kind):
    return array.ctypes.data_as(ct.POINTER(kind))

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--library', type=Path, required=True)
    parser.add_argument('--report', type=Path)
    args = parser.parse_args()
    reference = upstream()
    lib = ct.CDLL(str(args.library.resolve()))
    lib.deck_loops_rank.argtypes = [ct.POINTER(ct.c_double), ct.POINTER(ct.c_double),
        ct.POINTER(ct.c_uint32), ct.POINTER(Options), ct.POINTER(Pair), ct.c_uint32,
        ct.POINTER(ct.c_uint32), ct.c_char_p, ct.c_uint32]
    rng = np.random.default_rng(20261019)
    reports = []
    for name, frames, stride, test, prune, mode in [
        ('repeat-pruned', 320, 4, 91, True, 'repeat'),
        ('repeat-unpruned', 320, 4, 91, False, 'repeat'),
        ('edge-padding', 23, 4, 71, False, 'repeat'),
        ('single-weight', 23, 4, 1, False, 'repeat'),
        ('silence-features', 64, 4, 17, True, 'silence'),
        ('dissimilar', 100, 3, 31, True, 'random')]:
        base = rng.uniform(.05, .95, (4, 12))
        chroma = np.ascontiguousarray(np.clip(base[np.arange(frames) % 4] + rng.normal(0, .012, (frames, 12)), 0, 1))
        if mode == 'silence': chroma[:] = 0
        if mode == 'random': chroma[:] = rng.uniform(0, 1, chroma.shape)
        power = np.ascontiguousarray(rng.uniform(-1, -.25, frames))
        beats = np.arange(0, frames, stride, dtype=np.uint32)
        options = Options(1, frames, len(beats), stride, frames // 2, test, not prune, 100000, 10000000, 100000000)
        # Upstream expects [pitch,frame] and frequency-bin power, not max-bin dB.
        pairs = reference['_find_candidate_pairs'](chroma.T, power[None, :], beats.astype(np.int64), stride, frames // 2)
        pairs = [reference['LoopPair'](*pair) for pair in pairs]
        initial = len(pairs)
        if mode != 'random': assert initial > 0, name
        if len(pairs) >= 100 and prune:
            pairs = reference['_prune_candidates'](pairs)
        weights = reference['_weights'](test, start=max(2, test // 12), stop=1)
        for pair in pairs:
            pair.score = reference['_calculate_loop_score'](pair._loop_start_frame_idx, pair._loop_end_frame_idx, chroma.T, test, weights)
        pairs.sort(key=lambda pair: pair.score, reverse=True)
        result = (Pair * 100000)()
        count = ct.c_uint32()
        error = ct.create_string_buffer(256)
        status = lib.deck_loops_rank(pointer(chroma, ct.c_double), pointer(power, ct.c_double), pointer(beats, ct.c_uint32), ct.byref(options), result, len(result), ct.byref(count), error, len(error))
        assert status == 0, (status, error.value)
        assert count.value == len(pairs), (name, count.value, len(pairs))
        # Near-ties can change ordering with different floating-point reductions;
        # compare every candidate by identity and check score-sorted output.
        expected = {(p._loop_start_frame_idx, p._loop_end_frame_idx): p for p in pairs}
        maximum = 0
        for p in result[:count.value]:
            q = expected.pop((p.start, p.end))
            delta = max(abs(p.note - q.note_distance), abs(p.loudness - q.loudness_difference), abs(p.score - q.score))
            maximum = max(maximum, delta)
            assert delta < 1e-12, (name, p.start, p.end, delta)
        assert not expected
        assert all(result[i].score >= result[i + 1].score for i in range(count.value - 1))
        reports.append(dict(case=name, beforePruning=initial, candidates=count.value, maxAbsError=maximum))
    report = dict(scope='feature-input search/pruning/scoring only', cases=reports)
    if args.report: args.report.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report, indent=2))

if __name__ == '__main__': main()
