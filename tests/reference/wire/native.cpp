// The actual frozen client must be rejected by the native listener without admission.
#include "tent/runtime/control_plane.h"
#include <array>
#include <iostream>
#include <string>
using namespace mooncake::tent;
int main(int argc, char** argv) {
    if (argc != 4 || std::string(argv[1]) != "client") return 2;
    const int operation = std::stoi(argv[2]);
    const std::string peer(argv[3]);
    std::string description;
    std::array<char, 3> bytes{'a', 'b', 'c'};
    Status status;
    switch (operation) {
        case GetSegmentDesc: status = ControlClient::getSegmentDesc(peer, description); break;
        case SendData: status = ControlClient::sendData(peer, 0x0102030405060708ULL, bytes.data(), bytes.size()); break;
        case RecvData: status = ControlClient::recvData(peer, 0x0102030405060708ULL, bytes.data(), bytes.size()); break;
        default: return 2;
    }
    if (!status.ok()) { std::cerr << status.ToString() << '\n'; return 1; }
    return 0;
}
