def constant(): return 1
def fixed():
    for i in range(10): pass
def linear(n: int):
    for i in range(n): pass
def sequential(n: int, m: int):
    for i in range(n): pass
    for j in range(m): pass
def nested(n: int, m: int):
    for i in range(n):
        for j in range(m): pass
def triangular(n: int):
    for i in range(n):
        for j in range(i): pass
def quadratic(n: int):
    for i in range(n): linear(n)
def traversal(items: list[int]):
    for item in items: pass
def logarithmic(n: int):
    i = n
    while i > 0: i //= 2
def unresolved(n: int): foreign(n)
def recursive(n: int): recursive(n)
def changed(n: int):
    i = 0
    while i < n:
        n = 0
        i += 1
def conditional_step(n: int):
    i = 0
    while i < n:
        if i > 1: i += 1
def zero():
    for i in range(0): foreign()
