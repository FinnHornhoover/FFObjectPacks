#define PY_SSIZE_T_CLEAN
#include <Python.h>
#include <algorithm>
#include <cctype>
#include <cstdint>
#include <cstring>
#include <fstream>
#include <memory>
#include <set>
#include <stdexcept>
#include <string>
#include <string_view>
#include <unordered_map>
#include <variant>
#include <vector>

namespace bulk {
struct Owned {
    PyObject *p;
    explicit Owned(PyObject *value = nullptr) : p(value) {}
    ~Owned() { Py_XDECREF(p); }
};
struct Lock {
    PyGILState_STATE state = PyGILState_Ensure();
    ~Lock() { PyGILState_Release(state); }
};
struct Unlock {
    PyThreadState *state = PyEval_SaveThread();
    ~Unlock() { PyEval_RestoreThread(state); }
};
void require(bool valid, const char *message) {
    if (!valid)
        throw std::runtime_error(message);
}
std::string py_error() {
    PyObject *type, *value, *trace;
    PyErr_Fetch(&type, &value, &trace);
    Owned a(type), b(value), c(trace);
    Owned text(value ? PyObject_Str(value) : nullptr);
    std::string result = text.p ? PyUnicode_AsUTF8(text.p) : "Python callback failed";
    PyErr_Clear();
    return result;
}
std::string text(PyObject *value) {
    require(value != nullptr, "Missing Python value");
    const char *result = PyUnicode_AsUTF8(value);
    if (!result)
        throw std::runtime_error(py_error());
    return result;
}
long attribute(PyObject *value, const char *name) {
    Owned item(PyObject_GetAttrString(value, name));
    if (!item.p)
        throw std::runtime_error(py_error());
    long result = PyLong_AsLong(item.p);
    if (PyErr_Occurred())
        throw std::runtime_error(py_error());
    return result;
}
std::string lower(std::string s) {
    for (char &c : s)
        c = std::tolower(static_cast<unsigned char>(c));
    return s;
}
bool utf8(std::string_view s) {
    size_t i = 0;
    while (i < s.size()) {
        unsigned char c = s[i++];
        if (c < 128)
            continue;
        unsigned n = c >= 0xf0 ? 3 : c >= 0xe0 ? 2 : c >= 0xc2 ? 1 : 0;
        if (!n || c > 0xf4 || s.size() - i < n)
            return false;
        unsigned char first = s[i];
        if ((c == 0xe0 && first < 0xa0) || (c == 0xed && first >= 0xa0) ||
            (c == 0xf0 && first < 0x90) || (c == 0xf4 && first >= 0x90))
            return false;
        for (unsigned j = 0; j < n; ++j)
            if ((static_cast<unsigned char>(s[i++]) & 0xc0) != 0x80)
                return false;
    }
    return true;
}
struct Blob {
    std::string_view data;
};
struct Pointer {
    int64_t file, path;
};
struct Value {
    using Array = std::vector<Value>;
    using Map = std::vector<std::pair<std::string_view, Value>>;
    std::variant<std::monostate, bool, int64_t, double, std::string_view, Blob, Array, Map, Pointer>
        data;
    Value() = default;
    template <class T> explicit Value(T value) : data(std::move(value)) {}
    const Value *find(std::string_view name) const {
        auto *map = std::get_if<Map>(&data);
        if (map)
            for (auto &item : *map)
                if (item.first == name)
                    return &item.second;
        return nullptr;
    }
    const Value &at(std::string_view name) const {
        auto *item = find(name);
        if (!item)
            throw std::runtime_error("Missing field: " + std::string(name));
        return *item;
    }
    int64_t integer() const {
        if (auto *value = std::get_if<int64_t>(&data))
            return *value;
        if (auto *value = std::get_if<bool>(&data))
            return *value;
        throw std::runtime_error("Expected integer");
    }
    const Array &array() const { return std::get<Array>(data); }
    const Map &map() const { return std::get<Map>(data); }
};

enum class Op {
    Bool,
    S8,
    U8,
    S16,
    U16,
    S32,
    U32,
    S64,
    F32,
    F64,
    String,
    Pointer,
    Bytes,
    Array,
    Pair,
    Map
};
struct Node {
    std::string name;
    Op op = Op::Map;
    int expected = 0;
    bool align = false, extra_align = false;
    size_t fixed = 0;
    std::vector<std::unique_ptr<Node>> children;
};
std::unique_ptr<Node> compile(PyObject *tree, int format) {
    auto node = std::make_unique<Node>();
    Owned type(PyObject_GetAttrString(tree, "type")), name(PyObject_GetAttrString(tree, "name"));
    std::string t = text(type.p);
    node->name = text(name.p);
    node->expected = attribute(tree, "size");
    node->align = attribute(tree, "post_align");
    Owned children(PyObject_GetAttrString(tree, "children"));
    if (!children.p)
        throw std::runtime_error(py_error());
    Py_ssize_t count = PyList_Size(children.p);
    auto primitive = [&](Op op, size_t width) {
        node->op = op;
        node->fixed = width;
    };
    if (t == "bool")
        primitive(Op::Bool, 1);
    else if (t == "SInt8")
        primitive(Op::S8, 1);
    else if (t == "UInt8")
        primitive(Op::U8, 1);
    else if (t == "SInt16")
        primitive(Op::S16, 2);
    else if (t == "UInt16")
        primitive(Op::U16, 2);
    else if (t == "int" || t == "SInt32")
        primitive(Op::S32, 4);
    else if (t == "UInt32" || t == "unsigned int")
        primitive(Op::U32, 4);
    else if (t == "SInt64" || t == "UInt64")
        primitive(Op::S64, 8);
    else if (t == "float")
        primitive(Op::F32, 4);
    else if (t == "double")
        primitive(Op::F64, 8);
    else if (t == "string") {
        node->op = Op::String;
        node->extra_align = count && attribute(PyList_GetItem(children.p, 0), "post_align");
    } else if (t.rfind("PPtr<", 0) == 0)
        primitive(Op::Pointer, format >= 14 ? 12 : 8);
    else {
        bool array = attribute(tree, "is_array");
        PyObject *first = count ? PyList_GetItem(children.p, 0) : nullptr;
        if (array || (first && attribute(first, "is_array"))) {
            PyObject *holder = array ? tree : first;
            node->extra_align = attribute(holder, "post_align");
            Owned entries(PyObject_GetAttrString(holder, "children"));
            require(entries.p && PyList_Size(entries.p) >= 2, "Invalid array schema");
            PyObject *item = PyList_GetItem(entries.p, 1);
            Owned item_type(PyObject_GetAttrString(item, "type"));
            std::string item_name = text(item_type.p);
            if (item_name == "char" || item_name == "UInt8")
                node->op = Op::Bytes;
            else {
                node->op = Op::Array;
                node->children.push_back(compile(item, format));
            }
        } else {
            node->op = t == "pair" ? Op::Pair : Op::Map;
            size_t width = 0;
            bool fixed = count > 0;
            for (Py_ssize_t i = 0; i < count; ++i) {
                auto child = compile(PyList_GetItem(children.p, i), format);
                fixed &= child->fixed && !child->align;
                width += child->fixed;
                node->children.push_back(std::move(child));
            }
            node->fixed = fixed ? width : 0;
        }
    }
    return node;
}
struct Reader {
    const uint8_t *data;
    size_t size, pos = 0;
    int format;
    void advance(size_t bytes) {
        require(pos <= size && bytes <= size - pos, "Object exceeds serialized buffer");
        pos += bytes;
    }
    uint64_t integer(size_t bytes) {
        size_t start = pos;
        advance(bytes);
        uint64_t result = 0;
        for (size_t i = 0; i < bytes; ++i)
            result |= uint64_t(data[start + i]) << (8 * i);
        return result;
    }
    std::string_view bytes(size_t count) {
        size_t start = pos;
        advance(count);
        return {reinterpret_cast<const char *>(data + start), count};
    }
    Value read(const Node &node, bool keep = true, const std::set<std::string> *fields = nullptr) {
        size_t before = pos;
        Value result;
        if (!keep && node.fixed)
            advance(node.fixed);
        else
            switch (node.op) {
            case Op::Bool: {
                auto n = integer(1);
                if (keep)
                    result = Value(bool(n));
                break;
            }
            case Op::S8: {
                auto n = int8_t(integer(1));
                if (keep)
                    result = Value(int64_t(n));
                break;
            }
            case Op::U8: {
                auto n = integer(1);
                if (keep)
                    result = Value(int64_t(n));
                break;
            }
            case Op::S16: {
                auto n = int16_t(integer(2));
                if (keep)
                    result = Value(int64_t(n));
                break;
            }
            case Op::U16: {
                auto n = integer(2);
                if (keep)
                    result = Value(int64_t(n));
                break;
            }
            case Op::S32: {
                auto n = int32_t(integer(4));
                if (keep)
                    result = Value(int64_t(n));
                break;
            }
            case Op::U32: {
                auto n = integer(4);
                if (keep)
                    result = Value(int64_t(n));
                break;
            }
            case Op::S64: {
                auto n = int64_t(integer(8));
                if (keep)
                    result = Value(n);
                break;
            }
            case Op::F32: {
                uint32_t bits = integer(4);
                float n;
                std::memcpy(&n, &bits, 4);
                if (keep)
                    result = Value(double(n));
                break;
            }
            case Op::F64: {
                uint64_t bits = integer(8);
                double n;
                std::memcpy(&n, &bits, 8);
                if (keep)
                    result = Value(n);
                break;
            }
            case Op::String: {
                auto s = bytes(integer(4));
                if (keep)
                    result = utf8(s) ? Value(s) : Value(Blob{s});
                break;
            }
            case Op::Pointer: {
                uint32_t first = integer(4);
                int64_t file = int32_t(first), path;
                if (format == 7) {
                    file = first & 0xffff;
                    path = (uint64_t(first >> 16) << 32) | integer(4);
                } else
                    path = format >= 14 ? int64_t(integer(8)) : int32_t(integer(4));
                if (keep && (file || path))
                    result = Value(Pointer{file, path});
                break;
            }
            case Op::Bytes: {
                auto s = bytes(integer(4));
                if (keep)
                    result = Value(Blob{s});
                break;
            }
            case Op::Array: {
                size_t count = integer(4);
                const auto &item = *node.children[0];
                if (!keep && item.fixed && !item.align) {
                    require(pos <= size && count <= (size - pos) / item.fixed,
                            "Array exceeds serialized buffer");
                    advance(count * item.fixed);
                } else {
                    require(pos <= size && count <= size - pos, "Invalid serialized array length");
                    Value::Array values;
                    if (keep)
                        values.reserve(count);
                    for (size_t i = 0; i < count; ++i) {
                        auto value = read(item, keep);
                        if (keep)
                            values.push_back(std::move(value));
                    }
                    if (keep)
                        result = Value(std::move(values));
                }
                break;
            }
            case Op::Pair: {
                Value::Array values;
                for (auto &child : node.children) {
                    auto value = read(*child, keep);
                    if (keep)
                        values.push_back(std::move(value));
                }
                if (keep)
                    result = Value(std::move(values));
                break;
            }
            case Op::Map: {
                Value::Map values;
                if (keep)
                    values.reserve(node.children.size());
                for (auto &child : node.children) {
                    bool wanted = keep && (!fields || fields->count(child->name));
                    auto value = read(*child, wanted);
                    if (wanted)
                        values.emplace_back(child->name, std::move(value));
                }
                if (keep)
                    result = Value(std::move(values));
                break;
            }
            }
        require(node.expected <= 0 || pos - before >= size_t(node.expected),
                "Object is shorter than its type tree");
        if (node.align || node.extra_align)
            pos = (pos + 3) & ~size_t(3);
        return result;
    }
};

struct Writer {
    std::string data;
    void byte(uint8_t n) { data.push_back(char(n)); }
    void big(uint64_t n, size_t bytes) {
        for (size_t i = bytes; i; --i)
            byte(n >> (8 * (i - 1)));
    }
    void integer(int64_t n) {
        if (n >= 0) {
            if (n < 128)
                byte(n);
            else if (n <= 255) {
                byte(0xcc);
                big(n, 1);
            } else if (n <= 65535) {
                byte(0xcd);
                big(n, 2);
            } else if (uint64_t(n) <= 0xffffffffu) {
                byte(0xce);
                big(n, 4);
            } else {
                byte(0xcf);
                big(n, 8);
            }
        } else if (n >= -32)
            byte(uint8_t(n));
        else if (n >= -128) {
            byte(0xd0);
            big(n, 1);
        } else if (n >= -32768) {
            byte(0xd1);
            big(n, 2);
        } else if (n >= INT32_MIN) {
            byte(0xd2);
            big(n, 4);
        } else {
            byte(0xd3);
            big(n, 8);
        }
    }
    void nil() { byte(0xc0); }
    void boolean(bool n) { byte(n ? 0xc3 : 0xc2); }
    void array(size_t n) {
        if (n < 16)
            byte(0x90 | n);
        else if (n <= 65535) {
            byte(0xdc);
            big(n, 2);
        } else {
            byte(0xdd);
            big(n, 4);
        }
    }
    void map(size_t n) {
        if (n < 16)
            byte(0x80 | n);
        else if (n <= 65535) {
            byte(0xde);
            big(n, 2);
        } else {
            byte(0xdf);
            big(n, 4);
        }
    }
    void string(std::string_view s) {
        if (s.size() < 32)
            byte(0xa0 | s.size());
        else if (s.size() <= 255) {
            byte(0xd9);
            big(s.size(), 1);
        } else if (s.size() <= 65535) {
            byte(0xda);
            big(s.size(), 2);
        } else {
            byte(0xdb);
            big(s.size(), 4);
        }
        data.append(s);
    }
    void blob(std::string_view s) {
        if (s.size() <= 255) {
            byte(0xc4);
            big(s.size(), 1);
        } else if (s.size() <= 65535) {
            byte(0xc5);
            big(s.size(), 2);
        } else {
            byte(0xc6);
            big(s.size(), 4);
        }
        data.append(s);
    }
    void floating(double n, bool single) {
        if (single) {
            float value = n;
            uint32_t bits;
            std::memcpy(&bits, &value, 4);
            byte(0xca);
            big(bits, 4);
        } else {
            uint64_t bits;
            std::memcpy(&bits, &n, 8);
            byte(0xcb);
            big(bits, 8);
        }
    }
};
struct Key {
    uint32_t asset;
    int64_t path;
    bool operator==(const Key &o) const { return asset == o.asset && path == o.path; }
};
struct KeyHash {
    size_t operator()(const Key &k) const {
        return std::hash<int64_t>{}(k.path) ^ (size_t(k.asset) * 0x9e3779b97f4a7c15ull);
    }
};
struct Asset {
    std::string original, name, source, version;
    int format;
    bool scene;
    std::vector<std::string> refs;
    std::vector<uint32_t> roots;
};
struct Error {
    std::string kind, file, asset, message;
    int64_t path = 0, file_id = 0;
    int type = 0;
};
struct Object {
    PyObject *owner;
    int tid;
    int64_t path;
    size_t offset, size;
    Node *schema;
    std::string error, rig;
};
struct Context {
    std::ofstream nodes;
    PyObject *terrain, *publish;
    std::vector<std::string> names, paths;
    std::unordered_map<std::string, uint32_t> name_ids, path_ids;
    std::vector<Key> keys;
    std::unordered_map<Key, uint32_t, KeyHash> key_ids;
    std::vector<Asset> assets;
    std::vector<std::pair<std::string, int64_t>> prefabs;
    std::vector<uint32_t> terrains, clips, rigs;
    std::vector<Error> errors;
    std::vector<std::pair<std::string, uint64_t>> counts;
    Asset *current = nullptr;
    Context(const std::string &file, PyObject *t, PyObject *p)
        : nodes(file, std::ios::binary), terrain(Py_NewRef(t)), publish(Py_NewRef(p)) {
        require(bool(nodes), "Cannot open node output");
    }
    ~Context() {
        Py_XDECREF(terrain);
        Py_XDECREF(publish);
    }
    uint32_t oid(std::string name, int64_t path) {
        name = lower(std::move(name));
        auto [asset, added] = name_ids.emplace(name, names.size());
        if (added)
            names.push_back(std::move(name));
        Key key{asset->second, path};
        auto [entry, inserted] = key_ids.emplace(key, keys.size());
        if (inserted)
            keys.push_back(key);
        return entry->second;
    }
    int64_t pointer(Pointer p) {
        if (!p.path)
            return -1;
        int64_t index = p.file;
        if (index < 0)
            index += current->refs.size();
        if (index < 0 || size_t(index) >= current->refs.size()) {
            errors.push_back({"bad-reference", "", current->original, "", p.path, p.file});
            return oid("unresolved:" + current->original + ":" + std::to_string(p.file), p.path);
        }
        return oid(current->refs[index], p.path);
    }
    int64_t pointer(const Value &value) {
        if (auto *p = std::get_if<Pointer>(&value.data))
            return pointer(*p);
        require(std::holds_alternative<std::monostate>(value.data), "Expected object pointer");
        return -1;
    }
    void count(std::string key) {
        for (auto &item : counts)
            if (item.first == key) {
                ++item.second;
                return;
            }
        counts.emplace_back(std::move(key), 1);
    }
    void reference(Writer &w, int64_t n) {
        if (n < 0)
            w.nil();
        else
            w.integer(n);
    }
    void emit(Writer &w, const Value &v, bool single = false) {
        switch (v.data.index()) {
        case 0:
            w.nil();
            break;
        case 1:
            w.boolean(std::get<bool>(v.data));
            break;
        case 2:
            w.integer(std::get<int64_t>(v.data));
            break;
        case 3:
            w.floating(std::get<double>(v.data), single);
            break;
        case 4:
            w.string(std::get<std::string_view>(v.data));
            break;
        case 5:
            w.blob(std::get<Blob>(v.data).data);
            break;
        case 6: {
            auto &a = v.array();
            w.array(a.size());
            for (auto &item : a)
                emit(w, item, single);
            break;
        }
        case 7: {
            auto &m = v.map();
            w.map(m.size());
            for (auto &item : m) {
                w.string(item.first);
                emit(w, item.second, single);
            }
            break;
        }
        case 8:
            w.map(1);
            w.string("r");
            reference(w, pointer(std::get<Pointer>(v.data)));
            break;
        }
    }
    void field(Writer &w, const Value &v, const char *name, Value fallback = Value()) {
        auto *found = v.find(name);
        emit(w, found ? *found : fallback);
    }
    void vector(Writer &w, const Value &value, std::string_view axes) {
        w.array(axes.size());
        for (char axis : axes)
            emit(w, value.at(std::string_view(&axis, 1)));
    }
    std::string publish_clip(const std::string &data) {
        Lock lock;
        Owned bytes(PyBytes_FromStringAndSize(data.data(), data.size()));
        Owned result(PyObject_CallOneArg(publish, bytes.p));
        if (!result.p)
            throw std::runtime_error(py_error());
        return text(result.p);
    }
    std::string terrain_data(PyObject *obj) {
        Lock lock;
        Owned result(PyObject_CallOneArg(terrain, obj));
        if (!result.p)
            throw std::runtime_error(py_error());
        require(PyBytes_Check(result.p), "Terrain callback must return packed bytes");
        return {PyBytes_AS_STRING(result.p), size_t(PyBytes_GET_SIZE(result.p))};
    }
    void describe(Writer &w, const Value &raw, const Object &obj) {
        int tid = obj.tid;
        if (tid == 74) {
            Writer payload;
            emit(payload, raw, true);
            std::string file = publish_clip(payload.data);
            std::vector<std::string_view> curves;
            std::set<uint32_t> targets;
            for (auto &[name, tracks] : raw.map()) {
                if (name.size() < 6 || name.substr(name.size() - 6) != "Curves" ||
                    !std::holds_alternative<Value::Array>(tracks.data) || tracks.array().empty())
                    continue;
                curves.push_back(name);
                for (auto &track : tracks.array()) {
                    auto *path = track.find("path");
                    if (!path)
                        path = track.find("m_Path");
                    if (!path || !std::holds_alternative<std::string_view>(path->data))
                        continue;
                    std::string s(std::get<std::string_view>(path->data));
                    auto [it, added] = path_ids.emplace(s, paths.size());
                    if (added)
                        paths.push_back(std::move(s));
                    targets.insert(it->second);
                }
            }
            w.map(5);
            w.string("name");
            field(w, raw, "m_Name", Value(std::string_view("")));
            w.string("file");
            w.string(file);
            w.string("sample_rate");
            field(w, raw, "m_SampleRate");
            w.string("curve_types");
            w.array(curves.size());
            for (auto s : curves)
                w.string(s);
            w.string("paths");
            w.array(targets.size());
            for (auto id : targets)
                w.integer(id);
        } else if (!obj.rig.empty()) {
            w.map(raw.map().size() + 1);
            w.string("script_type");
            w.string(obj.rig);
            for (auto &[key, value] : raw.map()) {
                w.string(key);
                emit(w, value);
            }
        } else if (tid == 43 || tid == 48) {
            w.map(1);
            w.string("name");
            field(w, raw, "m_Name", Value(std::string_view("")));
        } else if (tid == 27 || tid == 28 || tid == 89) {
            w.map(5);
            w.string("name");
            field(w, raw, "m_Name", Value(std::string_view("")));
            w.string("width");
            field(w, raw, "m_Width");
            w.string("height");
            field(w, raw, "m_Height");
            w.string("format");
            field(w, raw, "m_TextureFormat");
            w.string("settings");
            field(w, raw, "m_TextureSettings", Value(Value::Map{}));
        } else if (tid == 1) {
            w.map(4);
            w.string("name");
            field(w, raw, "m_Name", Value(std::string_view("")));
            w.string("active");
            field(w, raw, "m_IsActive", Value(int64_t(1)));
            w.string("layer");
            field(w, raw, "m_Layer", Value(int64_t(0)));
            auto &components = raw.at("m_Component").array();
            size_t n = 0;
            for (auto &c : components)
                if (component(c.array()[0].integer()))
                    ++n;
            w.string("components");
            w.array(n);
            for (auto &c : components) {
                int64_t kind = c.array()[0].integer();
                if (!component(kind))
                    continue;
                w.array(2);
                w.integer(kind);
                reference(w, pointer(c.array()[1]));
            }
        } else if (tid == 4) {
            w.map(5);
            w.string("go");
            reference(w, pointer(raw.at("m_GameObject")));
            w.string("parent");
            reference(w, pointer(raw.at("m_Father")));
            w.string("p");
            vector(w, raw.at("m_LocalPosition"), "xyz");
            w.string("q");
            vector(w, raw.at("m_LocalRotation"), "xyzw");
            w.string("s");
            vector(w, raw.at("m_LocalScale"), "xyz");
        } else if (tid == 33) {
            w.map(2);
            w.string("go");
            reference(w, pointer(raw.at("m_GameObject")));
            w.string("mesh");
            reference(w, pointer(raw.at("m_Mesh")));
        } else
            emit(w, raw);
    }
    static bool component(int tid) {
        return tid < 0 || tid == 4 || tid == 23 || tid == 26 || tid == 33 || tid == 108 ||
               tid == 111 || tid == 137 || tid == 154;
    }
    static bool visual(int tid) {
        switch (tid) {
        case 1:
        case 4:
        case 21:
        case 23:
        case 26:
        case 27:
        case 28:
        case 33:
        case 43:
        case 48:
        case 74:
        case 89:
        case 104:
        case 108:
        case 111:
        case 137:
        case 154:
        case 156:
        case 157:
            return true;
        default:
            return false;
        }
    }
    static bool scene_root(int tid) {
        return tid == 23 || tid == 26 || tid == 104 || tid == 108 || tid == 111 || tid == 137 ||
               tid == 154 || tid == 157;
    }
    void process(const Object &obj, const uint8_t *buffer, size_t length, uint32_t aid) {
        int tid = obj.tid;
        bool supported = visual(tid) || !obj.rig.empty();
        if (!supported && tid != 142 && !(current->scene && tid < 0))
            return;
        int64_t id = -1;
        if (tid == 74) {
            id = oid(current->name, obj.path);
            clips.push_back(id);
        }
        if (!obj.rig.empty()) {
            id = oid(current->name, obj.path);
            rigs.push_back(id);
        }
        if (supported)
            id = oid(current->name, obj.path);
        try {
            if (!obj.schema)
                throw std::runtime_error(obj.error);
            Value raw;
            std::string terrain;
            if (tid == 156)
                terrain = terrain_data(obj.owner);
            else {
                require(obj.offset <= length && obj.size <= length - obj.offset,
                        "Object exceeds asset buffer");
                Reader reader{buffer + obj.offset, obj.size, 0, current->format};
                static const std::set<std::string> mesh = {"m_Name"};
                static const std::set<std::string> texture = {
                    "m_Name", "m_Width", "m_Height", "m_TextureFormat", "m_TextureSettings"};
                auto *fields = tid == 43                               ? &mesh
                               : (tid == 27 || tid == 28 || tid == 89) ? &texture
                                                                       : nullptr;
                raw = reader.read(*obj.schema, true, fields);
            }
            if (tid == 142) {
                for (auto &entry : raw.at("m_Container").array()) {
                    auto &pair = entry.array();
                    prefabs.emplace_back(std::string(std::get<std::string_view>(pair[0].data)),
                                         pointer(pair[1].at("asset")));
                }
                return;
            }
            if (!supported) {
                bool terrain = false;
                for (auto &item : raw.map())
                    if (lower(std::string(item.first)).find("terrain") != std::string::npos) {
                        terrain = true;
                        break;
                    }
                if (!terrain)
                    return;
                id = oid(current->name, obj.path);
            }
            Writer row;
            row.array(4);
            row.integer(id);
            row.integer(tid);
            row.integer(aid);
            if (tid == 156)
                row.data += terrain;
            else
                describe(row, raw, obj);
            nodes.write(row.data.data(), row.data.size());
            require(bool(nodes), "Cannot write node output");
            if (supported) {
                count(std::to_string(tid));
                if (tid == 156)
                    terrains.push_back(id);
                if (current->scene && scene_root(tid))
                    current->roots.push_back(id);
            } else
                current->roots.push_back(id);
        } catch (const std::exception &e) {
            if (tid == 142)
                throw;
            if (!supported) {
                count("unsupported_scene_scripts");
                return;
            }
            errors.push_back(
                {"object", current->source, current->name, e.what(), obj.path, 0, tid});
            if (tid == 74 || !obj.rig.empty())
                throw std::runtime_error("Cannot extract object " + std::to_string(obj.path) +
                                         " (type " + std::to_string(tid) + "): " + e.what());
        }
    }
    void error_record(Writer &w, const Error &e) {
        if (e.kind == "non-bundle") {
            w.map(2);
            w.string("kind");
            w.string(e.kind);
            w.string("file");
            w.string(e.file);
            return;
        }
        if (e.kind == "bad-reference") {
            w.map(4);
            w.string("kind");
            w.string(e.kind);
            w.string("asset");
            w.string(e.asset);
            w.string("file_id");
            w.integer(e.file_id);
            w.string("path_id");
            w.integer(e.path);
            return;
        }
        w.map(6);
        w.string("kind");
        w.string(e.kind);
        w.string("file");
        w.string(e.file);
        w.string("asset");
        w.string(e.asset);
        w.string("path_id");
        w.integer(e.path);
        w.string("type");
        w.integer(e.type);
        w.string("error");
        w.string(e.message);
    }
    std::string metadata(bool summary) {
        Writer w;
        w.map(summary ? 4 : 10);
        w.string("assets");
        w.array(assets.size());
        for (auto &a : assets) {
            w.map(5);
            w.string("name");
            w.string(a.name);
            w.string("source");
            w.string(a.source);
            w.string("scene");
            w.boolean(a.scene);
            w.string("version");
            w.string(a.version);
            w.string("format");
            w.integer(a.format);
        }
        if (!summary) {
            w.string("keys");
            w.array(keys.size());
            for (auto &key : keys) {
                w.array(2);
                w.string(names[key.asset]);
                w.integer(key.path);
            }
            w.string("scene_roots");
            size_t n = 0;
            for (auto &a : assets)
                if (!a.roots.empty())
                    ++n;
            w.array(n);
            for (size_t i = 0; i < assets.size(); ++i)
                if (!assets[i].roots.empty()) {
                    w.map(2);
                    w.string("asset");
                    w.integer(i);
                    w.string("roots");
                    w.array(assets[i].roots.size());
                    for (auto id : assets[i].roots)
                        w.integer(id);
                }
        }
        w.string("terrain_roots");
        w.array(terrains.size());
        for (auto id : terrains)
            w.integer(id);
        if (!summary) {
            w.string("clip_roots");
            w.array(clips.size());
            for (auto id : clips)
                w.integer(id);
            w.string("rig_roots");
            w.array(rigs.size());
            for (auto id : rigs)
                w.integer(id);
            w.string("animation_paths");
            w.array(paths.size());
            for (auto &p : paths)
                w.string(p);
            w.string("prefab_roots");
            w.array(prefabs.size());
            for (auto &p : prefabs) {
                w.array(2);
                w.string(p.first);
                reference(w, p.second);
            }
        }
        w.string("errors");
        w.array(errors.size());
        for (auto &e : errors)
            error_record(w, e);
        w.string("counts");
        w.map(counts.size());
        for (auto &c : counts) {
            w.string(c.first);
            w.integer(c.second);
        }
        return std::move(w.data);
    }
};

Context *context(PyObject *obj) {
    return static_cast<Context *>(PyCapsule_GetPointer(obj, "ffobjects.scan"));
}
void destroy(PyObject *obj) { delete context(obj); }
PyObject *create(PyObject *, PyObject *args) {
    const char *file;
    PyObject *terrain, *publish;
    if (!PyArg_ParseTuple(args, "sOO", &file, &terrain, &publish))
        return nullptr;
    try {
        return PyCapsule_New(new Context(file, terrain, publish), "ffobjects.scan", destroy);
    } catch (const std::exception &e) {
        PyErr_SetString(PyExc_RuntimeError, e.what());
        return nullptr;
    }
}
PyObject *scan_asset(PyObject *, PyObject *args) {
    PyObject *capsule, *asset;
    const char *source, *version;
    if (!PyArg_ParseTuple(args, "OOss", &capsule, &asset, &source, &version))
        return nullptr;
    auto *ctx = context(capsule);
    if (!ctx)
        return nullptr;
    try {
        Owned name(PyObject_GetAttrString(asset, "name"));
        Asset rec;
        rec.original = text(name.p);
        rec.name = lower(rec.original);
        rec.source = source;
        rec.version = version;
        rec.format = attribute(asset, "format");
        rec.scene =
            rec.name.rfind("buildplayer-", 0) == 0 &&
            !(rec.name.size() >= 13 && rec.name.substr(rec.name.size() - 13) == ".sharedassets");
        Owned refs(PyObject_GetAttrString(asset, "asset_refs"));
        require(refs.p && PyList_Check(refs.p), "Missing asset references");
        for (Py_ssize_t i = 0; i < PyList_Size(refs.p); ++i) {
            if (!i) {
                rec.refs.push_back(rec.original);
                continue;
            }
            Owned path(PyObject_GetAttrString(PyList_GetItem(refs.p, i), "file_path"));
            rec.refs.push_back(text(path.p));
        }
        uint32_t aid = ctx->assets.size();
        ctx->assets.push_back(std::move(rec));
        ctx->current = &ctx->assets.back();
        Owned objects(PyObject_GetAttrString(asset, "objects"));
        require(objects.p && PyDict_Check(objects.p), "Missing object table");
        std::unordered_map<int, std::unique_ptr<Node>> schemas;
        std::unordered_map<int, std::string> failures;
        std::vector<Object> entries;
        entries.reserve(PyDict_Size(objects.p));
        Py_ssize_t cursor = 0;
        PyObject *key, *info;
        while (PyDict_Next(objects.p, &cursor, &key, &info)) {
            Owned d(PyObject_GetAttrString(info, "__dict__"));
            require(d.p != nullptr, "Missing object metadata");
            auto get = [&](const char *name) {
                auto *v = PyDict_GetItemString(d.p, name);
                require(v != nullptr, "Incomplete object metadata");
                auto n = PyLong_AsLongLong(v);
                if (PyErr_Occurred())
                    throw std::runtime_error(py_error());
                return n;
            };
            int tid = get("type_id");
            if (tid >= 0 && !Context::visual(tid) && tid != 142)
                continue;
            if (!schemas.count(tid) && !failures.count(tid)) {
                Owned tree(PyObject_GetAttrString(info, "type_tree"));
                if (!tree.p)
                    failures[tid] = py_error();
                else
                    try {
                        schemas[tid] = compile(tree.p, ctx->current->format);
                    } catch (const std::exception &e) {
                        failures[tid] = e.what();
                        PyErr_Clear();
                    }
            }
            Object o{info,
                     tid,
                     get("path_id"),
                     size_t(get("data_offset")),
                     size_t(get("size")),
                     schemas.count(tid) ? schemas[tid].get() : nullptr,
                     failures.count(tid) ? failures[tid] : "",
                     ""};
            if (tid < 0 && o.schema) {
                std::set<std::string> fields;
                for (auto &c : o.schema->children)
                    fields.insert(c->name);
                if (fields.count("actorBones"))
                    o.rig = "ActorSkinCombiner";
                else if (fields.count("transformIndicesM") && fields.count("transformIndicesF"))
                    o.rig = "ActorWearIndexTable";
                else if (fields.count("skins"))
                    o.rig = "ActorWearSkinTable";
            }
            entries.push_back(std::move(o));
        }
        Owned reader(PyObject_GetAttrString(asset, "_buf"));
        require(reader.p != nullptr, "Missing serialized reader");
        Owned buf(PyObject_GetAttrString(reader.p, "buf"));
        require(buf.p != nullptr, "Missing serialized buffer");
        Owned view(PyObject_CallMethod(buf.p, "getbuffer", nullptr));
        require(view.p != nullptr, "Missing serialized buffer");
        size_t offset = attribute(asset, "_buf_ofs");
        Py_buffer memory;
        if (PyObject_GetBuffer(view.p, &memory, PyBUF_SIMPLE) < 0)
            throw std::runtime_error(py_error());
        try {
            Unlock unlocked;
            for (auto &o : entries) {
                o.offset += offset;
                ctx->process(o, static_cast<const uint8_t *>(memory.buf), memory.len, aid);
            }
        } catch (...) {
            PyBuffer_Release(&memory);
            throw;
        }
        PyBuffer_Release(&memory);
        Py_RETURN_NONE;
    } catch (const std::exception &e) {
        if (!PyErr_Occurred())
            PyErr_SetString(PyExc_RuntimeError, e.what());
        return nullptr;
    }
}
PyObject *pointer(PyObject *, PyObject *args) {
    PyObject *capsule;
    long long file, path;
    if (!PyArg_ParseTuple(args, "OLL", &capsule, &file, &path))
        return nullptr;
    auto *ctx = context(capsule);
    if (!ctx)
        return nullptr;
    try {
        auto id = ctx->pointer(Pointer{file, path});
        if (id < 0)
            Py_RETURN_NONE;
        return PyLong_FromLongLong(id);
    } catch (const std::exception &e) {
        PyErr_SetString(PyExc_RuntimeError, e.what());
        return nullptr;
    }
}
PyObject *non_bundle(PyObject *, PyObject *args) {
    PyObject *capsule;
    const char *file;
    if (!PyArg_ParseTuple(args, "Os", &capsule, &file))
        return nullptr;
    auto *ctx = context(capsule);
    if (!ctx)
        return nullptr;
    ctx->errors.push_back({"non-bundle", file});
    Py_RETURN_NONE;
}
PyObject *finish(PyObject *, PyObject *args) {
    PyObject *capsule;
    const char *path;
    if (!PyArg_ParseTuple(args, "Os", &capsule, &path))
        return nullptr;
    auto *ctx = context(capsule);
    if (!ctx)
        return nullptr;
    try {
        ctx->nodes.close();
        require(bool(ctx->nodes), "Cannot finish node output");
        auto bytes = ctx->metadata(false);
        std::ofstream out(path, std::ios::binary);
        out.write(bytes.data(), bytes.size());
        require(bool(out), "Cannot write scan metadata");
        out.close();
        require(bool(out), "Cannot finish scan metadata");
        auto summary = ctx->metadata(true);
        Py_CLEAR(ctx->terrain);
        Py_CLEAR(ctx->publish);
        return PyBytes_FromStringAndSize(summary.data(), summary.size());
    } catch (const std::exception &e) {
        PyErr_SetString(PyExc_RuntimeError, e.what());
        return nullptr;
    }
}
PyObject *stats(PyObject *, PyObject *args) {
    PyObject *capsule;
    if (!PyArg_ParseTuple(args, "O", &capsule))
        return nullptr;
    auto *ctx = context(capsule);
    if (!ctx)
        return nullptr;
    return Py_BuildValue("(nnnn)", Py_ssize_t(ctx->assets.size()), Py_ssize_t(ctx->clips.size()),
                         Py_ssize_t(ctx->rigs.size()), Py_ssize_t(ctx->errors.size()));
}
PyMethodDef methods[] = {{"create", create, METH_VARARGS, nullptr},
                         {"scan_asset", scan_asset, METH_VARARGS, nullptr},
                         {"pointer", pointer, METH_VARARGS, nullptr},
                         {"non_bundle", non_bundle, METH_VARARGS, nullptr},
                         {"finish", finish, METH_VARARGS, nullptr},
                         {"stats", stats, METH_VARARGS, nullptr},
                         {nullptr, nullptr, 0, nullptr}};
PyModuleDef module = {PyModuleDef_HEAD_INIT, "_scan", nullptr, -1, methods};
} // namespace bulk
PyMODINIT_FUNC PyInit__scan() { return PyModule_Create(&bulk::module); }
