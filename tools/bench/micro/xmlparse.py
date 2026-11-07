# (perf6) xml_etree's parse step: etree.fromstring of the benchmark's generated document.
# usage: xmlparse.py N KIND   KIND: fromstring | iterparse | tostring | build
import io
import sys
import xml.etree.ElementTree as etree


def build_xml_tree():
    SubElement = etree.SubElement
    root = etree.Element('root')
    for c in range(50):
        child = SubElement(root, 'child-%d' % c, tag_type="child")
        for i in range(100):
            SubElement(child, 'subchild').text = 'LEAF-%d-%d' % (c, i)
    deep = SubElement(root, 'deepchildren', tag_type="deepchild")
    for i in range(50):
        deep = SubElement(deep, 'deepchild')
    SubElement(deep, 'deepleaf', tag_type="leaf").text = "LEAF"
    root.set('nb-elems', str(sum(1 for elem in root.iter())))
    return root


def run(n, kind):
    root = build_xml_tree()
    data = etree.tostring(root)
    # n counts elements processed (5154 per document)
    reps = max(1, n // 5154)
    if kind == 'fromstring':
        for _ in range(reps):
            etree.fromstring(data)
    elif kind == 'iterparse':
        for _ in range(reps):
            [(ev, el.tag) for ev, el in etree.iterparse(io.BytesIO(data), ('start', 'end'))]
    elif kind == 'tostring':
        for _ in range(reps):
            etree.tostring(root)
    elif kind == 'build':
        for _ in range(reps):
            build_xml_tree()
    else:
        raise SystemExit('unknown kind ' + kind)


run(int(sys.argv[1]), sys.argv[2])
