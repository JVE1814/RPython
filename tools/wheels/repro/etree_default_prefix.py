# Root cause (pandas io/xml test_default_namespace[etree]): ElementTree serialization
# treated a URI registered with the empty prefix (register_namespace("", uri)) like an
# unknown one and wrote ns0:...; it is the default namespace (xmlns="uri").
import xml.etree.ElementTree as ET

ET.register_namespace("", "http://example.com/default")
root = ET.Element("{http://example.com/default}data")
ET.SubElement(root, "{http://example.com/default}row").text = "1"
out = ET.tostring(root, encoding="unicode")
assert out == '<data xmlns="http://example.com/default"><row>1</row></data>', out
print("OK")
