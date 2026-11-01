# Per-sub-benchmark timing of pyperformance's bm_xml_etree with our interpreter.
# usage: python3 etree_bench.py   (env B="parse iterparse generate process", N=loops)
import importlib.util
import os
import sys

BM = ('/opt/bench/dl/pyperformance-1.14.0/pyperformance/data-files/'
      'benchmarks/bm_xml_etree/run_benchmark.py')
spec = importlib.util.spec_from_file_location('bm', BM)
bm = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bm)

import xml.etree.ElementTree as et
print('accelerated:', et.Element is not et._Element_Py)
for name in os.environ.get('B', 'parse iterparse generate process').split():
    func = getattr(bm, 'bench_' + name)
    t = bm.bench_etree(int(os.environ.get('N', '1')), et, func)
    print(name, round(t, 4))
