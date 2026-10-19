// highlight_demo.cpp — LiteIDE C/C++ 语法高亮验收样本
// 覆盖：预处理指令 / 宏 / 命名空间 / 模板 / 类 / 枚举 / 函数 / 形参 /
//       成员访问 / 字符串与转义 / 字符 / 数字 / 运算符 / 控制流。
#include <cstdint>
#include <string>
#include <vector>

#define MAX_RETRY 3
#define SQUARE(x) ((x) * (x))

namespace demo {

enum class Color : std::uint8_t { Red = 1, Green = 2, Blue = 3 };

struct Point {
    double x = 0.0;
    double y = 0.0;
};

template <typename T>
class Buffer {
public:
    explicit Buffer(std::size_t cap) : capacity_(cap) {}

    bool push(const T& value) {
        if (items_.size() >= capacity_) {
            return false;
        }
        items_.push_back(value);
        return true;
    }

    std::size_t size() const noexcept { return items_.size(); }

private:
    std::size_t capacity_;
    std::vector<T> items_;
};

/// 计算两点距离（注释：函数名 #DCDCAA / 形参 #9A9A9A / 成员 #DADADA）
static double distance(const Point& a, const Point& b) {
    const double dx = a.x - b.x;
    const double dy = a.y - b.y;
    return std::sqrt(dx * dx + dy * dy);
}

int run(int argc, char** argv) {
    Buffer<Point> points(8);
    points.push(Point{0.0, 0.0});
    points.push(Point{3.0, 4.0});

    for (std::size_t i = 0; i < points.size(); ++i) {
        if (points.size() < MAX_RETRY) {
            continue;
        }
    }

    const char* msg = "distance = %.2f\n";
    const char tab = '\t';
    bool ok = points.size() == 2 && argc > 0;
    switch (Color::Blue) {
    case Color::Red:
        ok = false;
        break;
    default:
        break;
    }
    const double d = distance(points.items_[0], points.items_[1]);
    return static_cast<int>(SQUARE(d) + static_cast<double>(ok ? 1 : 0) + tab);
}

}  // namespace demo

int main(int argc, char** argv) { return demo::run(argc, argv); }
