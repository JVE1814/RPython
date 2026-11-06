# Values cached in registers must survive templates that jump by themselves (a
# PopJumpIf template branching to a block that uses them; found in bm_go).
import random
random.seed(1)
PASS = -1

class Board:
    def __init__(self):
        self.n = 0
    def useful(self, pos):
        self.n += 1
        return pos % 7 == 3

class EmptySet:
    def __init__(self, board):
        self.board = board
        self.empties = list(range(81))
        self.empty_pos = list(range(81))

    def random_choice(self):
        choices = len(self.empties)
        while choices:
            i = int(random.random() * choices)
            pos = self.empties[i]
            if self.board.useful(pos):
                return pos
            choices -= 1
            self.set(i, self.empties[choices])
            self.set(choices, pos)
        return PASS

    def set(self, i, pos):
        self.empties[i] = pos
        self.empty_pos[pos] = i

b = Board()
e = EmptySet(b)
t = 0
for k in range(3000):
    t += e.random_choice()
print(t, b.n)
