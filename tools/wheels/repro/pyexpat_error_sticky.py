# Root cause (lxml's test_elementtree.test_feed_parser_error_broken, run against the stdlib
# ElementTree): after a parse error, expat reports that error again on every later Parse()
# call; our engine returned success, so XMLParser.close() after a failed feed() did not
# raise ParseError.
import pyexpat
import xml.etree.ElementTree as ET

p = pyexpat.ParserCreate()
p.Parse('<?xml version="1.0"?><ro', False)
codes = []
for chunk, final in (("<><><>", False), ("", True), ("more", False)):
    try:
        p.Parse(chunk, final)
    except pyexpat.ExpatError as e:
        codes.append((e.code, e.lineno, e.offset))
assert len(codes) == 3 and len(set(codes)) == 1, codes

parser = ET.XMLParser()
parser.feed('<?xml version="1.0"?><ro')
try:
    parser.feed("<><><><>")
except ET.ParseError:
    pass
try:
    parser.close()
except ET.ParseError:
    pass
else:
    raise AssertionError("close() after a parse error did not raise")
print("OK")
