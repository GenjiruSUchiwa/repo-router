"""Surface fixture for the generic tags extractor."""

MODULE_VALUE = 1

class Service:
    """Service documentation mentions helper and result."""

    class_value = 2

    @staticmethod
    def run(value):
        result = helper(value)

        def nested():
            return hidden()

        return result


def top(value):
    return helper(value)


class Registry:
    """Registry documentation mentions store and lookup."""

    def __init__(self, store):
        self._store = store

    @property
    def size(self):
        """How many entries the registry holds."""
        return self._store.count()

    @size.setter
    def size(self, value):
        self._store.resize(value)


def lookup(registry, key):
    return registry.find(key)
