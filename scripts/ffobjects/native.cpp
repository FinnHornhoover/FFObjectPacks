#define PY_SSIZE_T_CLEAN
#include <Python.h>
#include <cstdint>
#include <cstring>
#include <memory>
#include <stdexcept>
#include <string>
#include <vector>

struct Owned {
    PyObject* p;
    explicit Owned(PyObject* value = nullptr) : p(value) {}
    ~Owned() { Py_XDECREF(p); }
    PyObject* release() { auto* value = p; p = nullptr; return value; }
};

static void check(bool condition, const char* message) {
    if (!condition) throw std::runtime_error(message);
}

struct Node {
    std::string type;
    PyObject *name = nullptr, *tree = nullptr, *wrapper = nullptr;
    int size = 0;
    bool align = false, array = false;
    std::vector<std::unique_ptr<Node>> children;
    ~Node() { Py_XDECREF(name); Py_XDECREF(tree); Py_XDECREF(wrapper); }
};

static long attribute_int(PyObject* obj, const char* name) {
    Owned value(PyObject_GetAttrString(obj, name));
    check(value.p != nullptr, "Missing type-tree attribute");
    long result = PyLong_AsLong(value.p);
    check(!PyErr_Occurred(), "Invalid type-tree integer");
    return result;
}

static std::unique_ptr<Node> compile_node(PyObject* tree, PyObject* engine) {
    auto n = std::make_unique<Node>();
    Owned type(PyObject_GetAttrString(tree, "type"));
    check(type.p != nullptr, "Missing type name");
    const char* text = PyUnicode_AsUTF8(type.p);
    check(text != nullptr, "Invalid type name");
    n->type = text;
    n->name = PyObject_GetAttrString(tree, "name");
    check(n->name != nullptr, "Missing field name");
    n->size = attribute_int(tree, "size");
    n->align = attribute_int(tree, "post_align");
    n->array = attribute_int(tree, "is_array");
    n->tree = Py_NewRef(tree);
    n->wrapper = PyObject_GetAttrString(engine, text);
    if (!n->wrapper) PyErr_Clear();
    Owned children(PyObject_GetAttrString(tree, "children"));
    check(children.p && PyList_Check(children.p), "Invalid type-tree children");
    for (Py_ssize_t i = 0; i < PyList_GET_SIZE(children.p); ++i)
        n->children.push_back(compile_node(PyList_GET_ITEM(children.p, i), engine));
    return n;
}

struct Reader {
    const uint8_t* data;
    size_t length, pos = 0;
    int format;
    bool wrap;
    PyObject *asset, *pointer_class, *owner;

    void advance(size_t count) {
        check(pos <= length && count <= length - pos, "Object exceeds serialized buffer");
        pos += count;
    }
    uint64_t integer(size_t bytes) {
        auto start = pos; advance(bytes);
        uint64_t result = 0;
        for (size_t i = 0; i < bytes; ++i) result |= uint64_t(data[start + i]) << (8 * i);
        return result;
    }
    // Legacy objects can omit the final alignment bytes. Later reads still check bounds.
    void align() { pos = (pos + 3) & ~size_t(3); }
    size_t fixed(const Node& n) {
        const auto& t = n.type;
        if (t == "bool" || t == "SInt8" || t == "UInt8") return 1;
        if (t == "SInt16" || t == "UInt16") return 2;
        if (t == "int" || t == "SInt32" || t == "UInt32" || t == "unsigned int" || t == "float") return 4;
        if (t == "SInt64" || t == "UInt64" || t == "double") return 8;
        if (t.rfind("PPtr<", 0) == 0) return format >= 14 ? 12 : 8;
        if (t == "string" || n.array || n.children.empty()) return 0;
        size_t result = 0;
        for (auto& child : n.children) {
            size_t width = fixed(*child);
            if (!width || child->align || child->array) return 0;
            result += width;
        }
        return result;
    }
    PyObject* read(const Node& n, bool keep = true, PyObject* fields = nullptr) {
        const size_t before = pos;
        const auto& t = n.type;
        bool extra_align = false;
        Owned result;
        const size_t width = keep ? 0 : fixed(n);
        if (width) advance(width);
        else if (t == "bool") { auto v = integer(1); if (keep) result.p = PyBool_FromLong(v != 0); }
        else if (t == "SInt8") { auto v = int8_t(integer(1)); if (keep) result.p = PyLong_FromLong(v); }
        else if (t == "UInt8") { auto v = integer(1); if (keep) result.p = PyLong_FromUnsignedLong(v); }
        else if (t == "SInt16") { auto v = int16_t(integer(2)); if (keep) result.p = PyLong_FromLong(v); }
        else if (t == "UInt16") { auto v = integer(2); if (keep) result.p = PyLong_FromUnsignedLong(v); }
        else if (t == "SInt32" || t == "int") { auto v = int32_t(integer(4)); if (keep) result.p = PyLong_FromLong(v); }
        else if (t == "UInt32" || t == "unsigned int") { auto v = integer(4); if (keep) result.p = PyLong_FromUnsignedLong(v); }
        // Match UnityPack's signed UInt64 representation.
        else if (t == "SInt64" || t == "UInt64") { auto v = int64_t(integer(8)); if (keep) result.p = PyLong_FromLongLong(v); }
        else if (t == "float") {
            uint32_t bits = integer(4); float v; std::memcpy(&v, &bits, 4);
            if (keep) result.p = PyFloat_FromDouble(v);
        } else if (t == "double") {
            uint64_t bits = integer(8); double v; std::memcpy(&v, &bits, 8);
            if (keep) result.p = PyFloat_FromDouble(v);
        } else if (t == "string") {
            const size_t count = integer(4), start = pos; advance(count);
            if (keep) {
                result.p = PyUnicode_DecodeUTF8(reinterpret_cast<const char*>(data + start), count, "strict");
                if (!result.p && PyErr_ExceptionMatches(PyExc_UnicodeDecodeError)) {
                    PyErr_Clear(); result.p = PyBytes_FromStringAndSize(reinterpret_cast<const char*>(data + start), count);
                }
            }
            extra_align = !n.children.empty() && n.children[0]->align;
        } else if (t.rfind("PPtr<", 0) == 0) {
            uint32_t first = integer(4);
            int64_t file = int32_t(first), path;
            if (format == 7) {
                file = first & 0xffff;
                path = (uint64_t(first >> 16) << 32) | integer(4);
            } else path = format >= 14 ? int64_t(integer(8)) : int32_t(integer(4));
            if (keep) {
                if (!file && !path) result.p = Py_NewRef(Py_None);
                else {
                    result.p = PyObject_CallFunctionObjArgs(pointer_class, n.tree, asset, nullptr);
                    check(result.p != nullptr, "Cannot create object pointer");
                    Owned f(PyLong_FromLongLong(file)), p(PyLong_FromLongLong(path));
                    check(PyObject_SetAttrString(result.p, "file_id", f.p) == 0 && PyObject_SetAttrString(result.p, "path_id", p.p) == 0, "Cannot populate object pointer");
                }
            }
        } else if (n.array || (!n.children.empty() && n.children[0]->array)) {
            const Node& array = n.array ? n : *n.children[0];
            check(array.children.size() >= 2, "Invalid array schema");
            const Node& item = *array.children[1];
            const size_t count = integer(4);
            extra_align = array.align;
            if (item.type == "char" || item.type == "UInt8") {
                const size_t start = pos; advance(count);
                if (keep) result.p = PyBytes_FromStringAndSize(reinterpret_cast<const char*>(data + start), count);
            } else {
                const size_t stride = fixed(item);
                if (!keep && stride && !item.align) {
                    check(count <= (length - pos) / stride, "Array exceeds serialized buffer");
                    advance(count * stride);
                } else {
                    check(count <= length - pos, "Invalid serialized array length");
                    if (keep) { result.p = PyList_New(count); check(result.p != nullptr, "Cannot allocate array"); }
                    for (size_t i = 0; i < count; ++i) {
                        Owned value(read(item, keep));
                        if (keep) PyList_SET_ITEM(result.p, i, value.release());
                    }
                }
            }
        } else if (t == "pair") {
            check(n.children.size() == 2, "Invalid pair schema");
            Owned first(read(*n.children[0], keep)), second(read(*n.children[1], keep));
            if (keep) result.p = PyTuple_Pack(2, first.p, second.p);
        } else {
            if (keep) { result.p = PyDict_New(); check(result.p != nullptr, "Cannot allocate object"); }
            for (auto& child : n.children) {
                bool wanted = keep;
                if (wanted && fields) {
                    int found = PySet_Contains(fields, child->name);
                    check(found >= 0, "Invalid projected fields"); wanted = found;
                }
                Owned value(read(*child, wanted));
                if (wanted) check(PyDict_SetItem(result.p, child->name, value.p) == 0, "Cannot populate object");
            }
            if (keep && wrap && n.wrapper) {
                Owned wrapped(PyObject_CallOneArg(n.wrapper, result.p));
                check(wrapped.p != nullptr, "Cannot wrap Unity object");
                Py_DECREF(result.p); result.p = wrapped.release();
                if (t == "StreamedResource" || t == "StreamingInfo") {
                    Owned source(PyObject_GetAttrString(result.p, t == "StreamedResource" ? "source" : "path"));
                    check(source.p != nullptr, "Missing stream source");
                    Owned target(PyObject_CallMethod(owner, "resolve_streaming_asset", "O", source.p));
                    check(target.p && PyObject_SetAttrString(result.p, "asset", target.p) == 0, "Cannot resolve stream source");
                }
            }
        }
        check(n.size <= 0 || pos - before >= size_t(n.size), "Object is shorter than its type tree");
        if (n.align || extra_align) align();
        if (keep) check(result.p != nullptr, "Cannot decode Unity value");
        return result.release();
    }
};

static void destroy_schema(PyObject* capsule) {
    delete static_cast<Node*>(PyCapsule_GetPointer(capsule, "ffobjects.schema"));
}
static PyObject* compile(PyObject*, PyObject* args) {
    PyObject *tree, *engine;
    if (!PyArg_ParseTuple(args, "OO", &tree, &engine)) return nullptr;
    try {
        auto n = compile_node(tree, engine);
        return PyCapsule_New(n.release(), "ffobjects.schema", destroy_schema);
    } catch (const std::exception& e) {
        if (!PyErr_Occurred()) PyErr_SetString(PyExc_ValueError, e.what());
        return nullptr;
    }
}
static PyObject* decode(PyObject*, PyObject* args) {
    PyObject *capsule, *binary, *asset, *pointer, *owner, *fields;
    int format, wrap;
    if (!PyArg_ParseTuple(args, "OOOOOiOp", &capsule, &binary, &asset, &pointer, &owner, &format, &fields, &wrap)) return nullptr;
    auto* schema = static_cast<Node*>(PyCapsule_GetPointer(capsule, "ffobjects.schema"));
    if (!schema) return nullptr;
    if (!PyBytes_Check(binary)) { PyErr_SetString(PyExc_TypeError, "Expected serialized bytes"); return nullptr; }
    Reader r{reinterpret_cast<const uint8_t*>(PyBytes_AS_STRING(binary)), size_t(PyBytes_GET_SIZE(binary)), 0, format, bool(wrap), asset, pointer, owner};
    try { return r.read(*schema, true, fields == Py_None ? nullptr : fields); }
    catch (const std::exception& e) {
        if (!PyErr_Occurred()) PyErr_SetString(PyExc_ValueError, e.what());
        return nullptr;
    }
}
static PyMethodDef methods[] = {
    {"compile", compile, METH_VARARGS, nullptr},
    {"decode", decode, METH_VARARGS, nullptr},
    {nullptr, nullptr, 0, nullptr}
};
static PyModuleDef module = {PyModuleDef_HEAD_INIT, "_native", nullptr, -1, methods};
PyMODINIT_FUNC PyInit__native() { return PyModule_Create(&module); }
