"""Exercise the actual headless binary on increasing ordinary/SNOT/tilted builds.

Run from cad/: python3 tools/smoke_instructions.py [--binary ~/.local/bin/cad]
All inputs and outputs are new files under one temporary directory.
"""
import argparse
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import numpy as np

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT.parent / 'bench-next'))
import ldraw


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=Path, default=ROOT/'target/debug/cad-app')
    args = parser.parse_args()
    binary = args.binary.expanduser().resolve()
    folder = Path(tempfile.mkdtemp(prefix='cad-instruction-smoke-'))
    lib = ldraw.get_library()
    stack = [ldraw.Ref('3005.dat', 4, np.array([0., -24.*i, 0.]), np.eye(3)) for i in range(5)]
    side = ldraw.flatten_model(ldraw.load_model_file(str(ROOT.parent/'bench-next/tests/connections/headlight_side_plate.ldr')), lib=lib)
    for i in range(1, 4):
        side.append(ldraw.Ref('3005.dat', 1, side[1].pos-side[1].rot[:, 1]*24*i, side[1].rot.copy()))
    theta = np.radians(37)
    rotation = np.array([[np.cos(theta), -np.sin(theta), 0.], [np.sin(theta), np.cos(theta), 0.], [0., 0., 1.]])
    tilted = [ldraw.Ref(p.name, p.color, rotation@p.pos, rotation@p.rot) for p in side]
    # Two completed independent assemblies must not acquire a fictitious join.
    multiple = side + [ldraw.Ref(p.name, p.color, p.pos+np.array([200., 0., 0.]), p.rot) for p in stack]
    shell = ldraw.flatten_model(ldraw.load_model_file(str(ROOT/'tests/fixtures/instruction_snot_shell.ldr')), lib=lib)
    hinges = ldraw.flatten_model(ldraw.load_model_file(str(ROOT/'tests/fixtures/parallel_hinges.ldr')), lib=lib)
    for name, refs in [('stack', stack), ('snot', side), ('tilted-snot', tilted), ('two-assemblies', multiple), ('snot-shell', shell), ('paired-hinges', hinges)]:
        source = folder/(name+'.ldr')
        source.write_text('0 Headless instruction smoke\n'+'\n'.join(p.line() for p in refs)+'\n')
        original = source.read_bytes()
        output = folder/name
        run = subprocess.run([str(binary), str(source), '--generate-instructions', '--output', str(output), '--image-size', '128'], cwd=folder, capture_output=True, text=True, timeout=120)
        assert run.returncode == 0, (name, run.stdout, run.stderr)
        result = json.loads(run.stdout)
        assert 'pages' not in result['result'], 'CLI should return counts, not hundreds of GUI callouts'
        plan = json.loads((output/'plan.json').read_text())
        assert plan['final_pose_check_passed'] and plan['assembly_path_found']
        assert sorted(i for s in plan['steps'] if s['kind'] == 'part' for i in s['added']) == list(range(len(refs)))
        assert (output/'instructions.html').is_file()
        assert all((output/f'step-{i:04}.png').is_file() for i in range(1, len(plan['steps'])+1))
        assert source.read_bytes() == original
        refused = subprocess.run([str(binary), str(source), '--generate-instructions', '--output', str(output)], capture_output=True, text=True, timeout=10)
        assert refused.returncode != 0, 'An existing output folder must never be overwritten'
        print(json.dumps({'pass': True, 'model': name, 'pieces': len(refs), 'steps': result['result']['steps'], 'output': str(output)}), flush=True)
    high_output=folder/'stack-high-quality'
    run=subprocess.run([str(binary),str(folder/'stack.ldr'),'--generate-instructions','--high-quality','--output',str(high_output),'--image-size','192'],cwd=folder,capture_output=True,text=True,timeout=120)
    assert run.returncode==0,(run.stdout,run.stderr)
    assert json.loads((high_output/'job.json').read_text())['high_quality'] is True
    from PIL import Image
    with Image.open(high_output/'step-0005.png') as image:assert image.size==(192,192)
    print(json.dumps({'pass':True,'mode':'high-quality','resolution':192}),flush=True)


if __name__ == '__main__':
    main()
