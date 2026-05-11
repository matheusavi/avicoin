import subprocess
import threading
import time

def test_rust_binary_runs():
    logs = []

    proc = subprocess.Popen(["./target/debug/avicoin"], stdout=subprocess.PIPE, text=True)

    def read_logs():
        for line in proc.stdout:
            print(line, end="")
            logs.append(line)

    t = threading.Thread(target=read_logs)
    t.start()

    time.sleep(16)

    proc.terminate()
    
    assert "Ping" in "".join(logs)
    assert "Pong" in "".join(logs)