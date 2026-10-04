import os
import sys

# 保证 pytest 可以直接导入项目根目录下的 app 包
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
