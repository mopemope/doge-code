import unittest
from settings import parse_timeout

class ExistingTimeoutTests(unittest.TestCase):
    def test_positive_seconds(self):
        self.assertEqual(parse_timeout(2), 2.0)
        self.assertEqual(parse_timeout(" 2.5 "), 2.5)

    def test_nonpositive_rejected(self):
        for value in (0, -1, "-0.5"):
            with self.subTest(value=value), self.assertRaises(ValueError):
                parse_timeout(value)

if __name__ == "__main__":
    unittest.main()
