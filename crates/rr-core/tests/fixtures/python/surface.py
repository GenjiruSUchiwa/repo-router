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
